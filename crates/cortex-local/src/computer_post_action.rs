//! 派发后结果未确认时，本轮电脑写入的隔离。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use cortex_agent::computer_driver::{DriverError, GatewayError};

use crate::computer_lease::ComputerLeaseRegistry;

/// 只约束同一个 Computer run，不是跨进程或跨 run 的持久全局账本。
#[derive(Debug)]
pub(crate) struct RunWriteGate {
    active: tokio::sync::Mutex<()>,
    state: tokio::sync::watch::Sender<GateState>,
    legacy_step: std::sync::Mutex<()>,
    closing: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    Open,
    Blocked,
    Closed,
}

impl Default for RunWriteGate {
    fn default() -> Self {
        let (state, _) = tokio::sync::watch::channel(GateState::Open);
        Self {
            active: tokio::sync::Mutex::new(()),
            state,
            legacy_step: std::sync::Mutex::new(()),
            closing: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl RunWriteGate {
    /// 持有 guard 直到动作和后置验证结束，防止其他 backend 在封锁前越过检查。
    pub(crate) async fn enter(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, &'static str> {
        const BLOCKED: &str =
            "本轮已有已派发或可能已派发、结果未确认的电脑动作；请人工核对，不要自动重试";
        const CLOSED: &str = "本轮电脑操作已关闭；在途动作的结果可能未知，请人工核对，不要自动重试";
        let mut state = self.state.subscribe();
        let result = |value| match value {
            GateState::Open => Ok(()),
            GateState::Blocked => Err(BLOCKED),
            GateState::Closed => Err(CLOSED),
        };
        result(*state.borrow())?;
        tokio::select! {
            biased;
            changed = async {
                state
                    .wait_for(|value| *value != GateState::Open)
                    .await
                    .map(|value| *value)
                    .map_err(|_| CLOSED)
            } => match changed {
                Ok(value) => match result(value) {
                    Err(reason) => Err(reason),
                    // predicate 保证这里不可达；若未来改动破坏该前提，仍按关闭处理。
                    Ok(()) => Err(CLOSED),
                },
                Err(reason) => Err(reason),
            },
            guard = self.active.lock() => {
                result(*self.state.borrow())?;
                Ok(guard)
            }
        }
    }

    pub(crate) fn block(&self) {
        self.state.send_if_modified(|state| {
            if *state == GateState::Open {
                *state = GateState::Blocked;
                true
            } else {
                false
            }
        });
    }

    pub(crate) fn close(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
        // 不等待阻塞中的同步输入；已经进入的单步视为在途，其余步骤检查 closing。
        self.state.send_replace(GateState::Closed);
    }

    pub(crate) async fn wait_for_close(&self) {
        let mut state = self.state.subscribe();
        let _ = state.wait_for(|state| *state == GateState::Closed).await;
    }

    pub(crate) fn is_blocked(&self) -> bool {
        self.closing.load(std::sync::atomic::Ordering::Acquire)
            || *self.state.borrow() != GateState::Open
    }
}

/// 字段按声明顺序析构：先丢弃动作 future 执行其取消保护，再释放写锁。
pub(crate) struct ProtectedCall<'a, F> {
    future: Pin<Box<F>>,
    _permit: Option<tokio::sync::MutexGuard<'a, ()>>,
}

impl<'a, F: Future> ProtectedCall<'a, F> {
    pub(crate) fn new(future: F, permit: Option<tokio::sync::MutexGuard<'a, ()>>) -> Self {
        Self {
            future: Box::pin(future),
            _permit: permit,
        }
    }
}

impl<F: Future> Future for ProtectedCall<'_, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}

/// future 被单独丢弃时仍同步检查派发边界；正常返回由调用者显式解除。
pub(crate) struct DispatchDropGuard<P: Fn() -> bool, F: FnOnce(bool)> {
    dispatched: P,
    on_drop: Option<F>,
}

impl<P: Fn() -> bool, F: FnOnce(bool)> DispatchDropGuard<P, F> {
    pub(crate) fn new(dispatched: P, on_drop: F) -> Self {
        Self {
            dispatched,
            on_drop: Some(on_drop),
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.on_drop = None;
    }
}

impl<P: Fn() -> bool, F: FnOnce(bool)> Drop for DispatchDropGuard<P, F> {
    fn drop(&mut self) {
        if let Some(on_drop) = self.on_drop.take() {
            on_drop((self.dispatched)());
        }
    }
}

pub(crate) fn is_write_tool(tool: &str) -> bool {
    matches!(
        tool,
        "computer_act" | "computer_browser_act" | "click" | "type_text" | "key" | "scroll"
    )
}

#[derive(Debug)]
pub(crate) enum LegacyInputError<E> {
    Failed(E),
    Panicked,
    Stopped,
}

impl<E: std::fmt::Display> std::fmt::Display for LegacyInputError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) => write!(formatter, "{error}"),
            Self::Panicked => formatter.write_str("输入操作异常中断"),
            Self::Stopped => formatter.write_str("本轮电脑输入已停止"),
        }
    }
}

/// panic 展开期间也必须先封锁；调用者只在输入 API 明确成功后解除。
struct LegacyInputGuard<'a> {
    gate: &'a RunWriteGate,
    armed: bool,
}

impl Drop for LegacyInputGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.gate.block();
        }
    }
}

/// legacy 输入可能部分成功；错误和 panic 都不能证明没有副作用。
pub(crate) fn legacy_input<T, E>(
    gate: &RunWriteGate,
    attempt: impl FnOnce() -> Result<T, E>,
) -> Result<T, LegacyInputError<E>> {
    legacy_input_step(gate, false, attempt)
}

fn legacy_input_step<T, E>(
    gate: &RunWriteGate,
    cleanup: bool,
    attempt: impl FnOnce() -> Result<T, E>,
) -> Result<T, LegacyInputError<E>> {
    let _step = gate
        .legacy_step
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cleanup && gate.is_blocked() {
        return Err(LegacyInputError::Stopped);
    }
    let mut guard = LegacyInputGuard { gate, armed: true };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(attempt)) {
        Ok(Ok(value)) => {
            guard.armed = false;
            Ok(value)
        }
        Ok(Err(error)) => Err(LegacyInputError::Failed(error)),
        Err(_) => Err(LegacyInputError::Panicked),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyKeyStep {
    Press,
    Click,
    Release,
}

/// 只释放本次尝试按下的修饰键，失败的按键也可能已送达；释放错误不妨碍后续清理。
pub(crate) fn legacy_chord<K: Copy, E>(
    gate: &RunWriteGate,
    modifiers: &[K],
    main: K,
    mut input: impl FnMut(K, LegacyKeyStep) -> Result<(), E>,
) -> Result<(), LegacyInputError<E>> {
    let mut pressed = Vec::with_capacity(modifiers.len());
    let mut failure = None;
    for &modifier in modifiers {
        match legacy_input(gate, || input(modifier, LegacyKeyStep::Press)) {
            Ok(()) => pressed.push(modifier),
            Err(error) => {
                // 未执行时不能释放可能属于用户物理输入的键。
                if !matches!(error, LegacyInputError::Stopped) {
                    pressed.push(modifier);
                }
                failure = Some(error);
                break;
            }
        }
    }
    if failure.is_none() {
        failure = legacy_input(gate, || input(main, LegacyKeyStep::Click)).err();
    }
    for &modifier in pressed.iter().rev() {
        if let Err(error) =
            legacy_input_step(gate, true, || input(modifier, LegacyKeyStep::Release))
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

pub(crate) fn browser_uncertain_message(error: &GatewayError, navigation: bool) -> &'static str {
    let action = if navigation { "导航" } else { "动作" };
    match (action, error) {
        (
            "导航",
            GatewayError::PostActionProtocol(_)
            | GatewayError::ClockWentBackwards
            | GatewayError::VerificationFailed { .. },
        ) => "浏览器导航已派发，但后置验证失败或不可用；禁止自动重试，请人工核对。",
        (
            _,
            GatewayError::PostActionProtocol(_)
            | GatewayError::ClockWentBackwards
            | GatewayError::VerificationFailed { .. },
        ) => "浏览器动作已派发，但后置验证失败或不可用；禁止自动重试，请人工核对。",
        ("导航", _) => "浏览器导航可能已派发，结果未知；禁止自动重试，请人工核对。",
        (_, _) => "浏览器动作可能已派发，结果未知；禁止自动重试，请人工核对。",
    }
}

/// 后置协议错误与验证失败发生在回执之后；OutcomeUnknown 也不能安全重放。
pub(crate) fn post_dispatch_unconfirmed(error: &GatewayError) -> bool {
    matches!(
        error,
        GatewayError::PostActionProtocol(_)
            | GatewayError::DurableFinish(_)
            | GatewayError::ClockWentBackwards
            | GatewayError::VerificationFailed { .. }
            | GatewayError::Driver(DriverError::OutcomeUnknown(_))
    )
}

/// 同步隔离，不得在保留租约和停用 driver 之前等待 session 清理。
pub(crate) fn stop_cua_if_unconfirmed(
    error: &GatewayError,
    gate: &RunWriteGate,
    leases: &Arc<ComputerLeaseRegistry>,
    run_id: &str,
    dispatched: bool,
    cancel: impl FnOnce(),
) -> bool {
    if !dispatched && !post_dispatch_unconfirmed(error) {
        return false;
    }
    gate.block();
    retain_unsafe_leases(leases, run_id);
    cancel();
    true
}

/// 浏览器是本轮独占实例；同步禁止新写入后由调用方异步关闭实例。
pub(crate) fn stop_browser_if_uncertain(
    error: &GatewayError,
    gate: &RunWriteGate,
    dispatched: bool,
    cancel: impl FnOnce(),
) -> bool {
    if !dispatched && !post_dispatch_unconfirmed(error) {
        return false;
    }
    gate.block();
    cancel();
    true
}

/// 没有可证明的清理结果时，持有 registry 至进程退出，防止命名对象提前释放。
pub(crate) fn retain_unsafe_leases(leases: &Arc<ComputerLeaseRegistry>, run_id: &str) {
    leases.quarantine_run(run_id);
    static RETAINED: std::sync::OnceLock<std::sync::Mutex<Vec<Arc<ComputerLeaseRegistry>>>> =
        std::sync::OnceLock::new();
    match RETAINED.get_or_init(Default::default).lock() {
        Ok(mut held) => {
            if !held.iter().any(|existing| Arc::ptr_eq(existing, leases)) {
                held.push(Arc::clone(leases));
            }
        }
        Err(_) => std::mem::forget(Arc::clone(leases)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_agent::computer_driver::{DriverError, GatewayError};
    use cortex_agent::tools::computer_protocol::ProtocolError;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn close_between_legacy_chord_steps_stops_input_and_releases_modifier() {
        let gate = Arc::new(RunWriteGate::default());
        let (pressed_tx, pressed_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let worker_gate = Arc::clone(&gate);
        let worker = std::thread::spawn(move || {
            legacy_chord(
                &worker_gate,
                &["ctrl"],
                "x",
                |key, step| -> Result<(), &'static str> {
                    events_tx.send((key, step)).expect("记录步骤");
                    if step == LegacyKeyStep::Press {
                        pressed_tx.send(()).expect("到达屏障");
                        resume_rx.recv().expect("恢复输入");
                    }
                    Ok(())
                },
            )
        });
        pressed_rx.recv().expect("修饰键正在执行");
        let close_gate = Arc::clone(&gate);
        let closing = std::thread::spawn(move || close_gate.close());
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !gate.closing.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < until, "关闭请求必须到达屏障");
            std::thread::yield_now();
        }
        resume_tx.send(()).expect("继续");
        assert!(
            worker.join().expect("线程").is_err(),
            "关闭后组合键不得成功"
        );
        closing.join().expect("关闭线程");
        let events: Vec<_> = events_rx.into_iter().collect();
        assert_eq!(
            events,
            vec![
                ("ctrl", LegacyKeyStep::Press),
                ("ctrl", LegacyKeyStep::Release)
            ],
            "关闭后不得点击主键，仍需尽力释放已按修饰键"
        );
    }

    fn unconfirmed_errors() -> Vec<GatewayError> {
        vec![
            GatewayError::PostActionProtocol(ProtocolError::InvalidFrameTtl),
            GatewayError::ClockWentBackwards,
            GatewayError::VerificationFailed {
                frame_id: "post-frame".into(),
                evidence: "条件不成立".into(),
            },
            GatewayError::Driver(DriverError::OutcomeUnknown("可能已执行".into())),
        ]
    }

    #[test]
    fn post_dispatch_errors_are_not_pre_dispatch_failures() {
        for error in unconfirmed_errors() {
            assert!(
                post_dispatch_unconfirmed(&error),
                "必须识别派发后未知结果：{error:?}"
            );
        }
        let before = GatewayError::Protocol(ProtocolError::InvalidFrameTtl);
        assert!(
            !post_dispatch_unconfirmed(&before),
            "派发前协议拒绝不能伪装成已派发"
        );
        assert!(
            !post_dispatch_unconfirmed(&GatewayError::PolicyDenied),
            "策略拒绝发生在派发前"
        );
    }

    #[tokio::test]
    async fn cua_retains_lease_and_blocks_the_run_before_cleanup_can_wait() {
        for (index, error) in unconfirmed_errors().into_iter().enumerate() {
            let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
            let gate = Arc::new(RunWriteGate::default());
            let window = format!("post-action-{}-{index}", std::process::id());
            leases
                .acquire_window("run-a", &window)
                .expect("先取得窗口租约");
            let active = gate.enter().await.expect("首个动作可写");
            let cancelled = AtomicBool::new(false);
            assert!(stop_cua_if_unconfirmed(
                &error,
                &gate,
                &leases,
                "run-a",
                true,
                || {
                    assert!(gate.is_blocked(), "停用 driver 前必须阻止本轮后续写入");
                    assert!(
                        leases.acquire_window("run-b", &window).is_err(),
                        "清理前必须保留租约"
                    );
                    assert!(
                        leases.acquire_window("run-a", "other-window").is_err(),
                        "隔离 run 不能换窗口"
                    );
                    cancelled.store(true, Ordering::Release);
                }
            ));
            assert!(cancelled.load(Ordering::Acquire), "必须同步停用 driver");
            leases.release_run("run-a");
            leases.release_window("run-a", &window);
            assert!(
                leases.acquire_window("run-b", &window).is_err(),
                "正常释放不能清掉隔离"
            );
            let waiting_gate = Arc::clone(&gate);
            let waiter = tokio::spawn(async move { waiting_gate.enter().await.is_err() });
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                    .await
                    .expect("封锁必须立即唤醒等待者")
                    .expect("等待任务完成"),
                "清理尚未结束时本轮写入也必须立即拒绝"
            );
            drop(active);
        }
    }

    #[tokio::test]
    async fn browser_stops_run_before_asynchronous_close() {
        for error in unconfirmed_errors() {
            let gate = RunWriteGate::default();
            let cancelled = AtomicBool::new(false);
            assert!(stop_browser_if_uncertain(&error, &gate, true, || {
                assert!(gate.is_blocked(), "关闭浏览器之前必须阻止同轮其他写入");
                cancelled.store(true, Ordering::Release);
            }));
            assert!(
                cancelled.load(Ordering::Acquire),
                "关闭前必须先取消 browser driver"
            );
            assert!(
                gate.enter().await.is_err(),
                "同一 run 不能切换到另一个 backend 继续写"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_cleanup_keeps_run_blocked_and_window_quarantined() {
        let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
        let gate = RunWriteGate::default();
        let window = format!("abandoned-cleanup-{}", std::process::id());
        leases.acquire_window("run-a", &window).expect("窗口租约");
        let cancelled = AtomicBool::new(false);
        let error = GatewayError::ClockWentBackwards;
        let mut cleanup = Box::pin(async {
            assert!(stop_cua_if_unconfirmed(
                &error,
                &gate,
                &leases,
                "run-a",
                true,
                || cancelled.store(true, Ordering::Release),
            ));
            std::future::pending::<()>().await;
        });
        tokio::select! {
            biased;
            _ = &mut cleanup => panic!("模拟清理不应自行完成"),
            _ = tokio::task::yield_now() => {}
        }
        drop(cleanup);
        assert!(
            cancelled.load(Ordering::Acquire),
            "等待清理前必须停用 driver"
        );
        assert!(
            gate.enter().await.is_err(),
            "清理 future 被丢弃后仍不能写入"
        );
        leases.release_run("run-a");
        assert!(
            leases.acquire_window("run-b", &window).is_err(),
            "清理中断不能释放窗口"
        );
        #[cfg(windows)]
        {
            drop(leases);
            let other = crate::computer_lease::ComputerLeaseRegistry::new();
            assert!(
                other.acquire_window("run-b", &window).is_err(),
                "隔离注册表必须存活到进程退出"
            );
        }
    }

    #[tokio::test]
    async fn dropping_one_write_future_only_blocks_after_dispatch() {
        for dispatched in [false, true] {
            let gate = Arc::new(RunWriteGate::default());
            let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
            let window = format!("drop-one-{}-{dispatched}", std::process::id());
            leases.acquire_window("run-a", &window).expect("窗口租约");
            let marker = AtomicBool::new(false);
            let stopped = AtomicBool::new(false);
            let audited = AtomicBool::new(false);
            let permit = gate.enter().await.expect("本轮首个写入");
            let call = async {
                let _drop_guard = DispatchDropGuard::new(
                    || marker.load(Ordering::Acquire),
                    |was_dispatched| {
                        assert!(gate.active.try_lock().is_err(), "取消保护必须先于写锁释放");
                        if was_dispatched {
                            gate.block();
                            retain_unsafe_leases(&leases, "run-a");
                            stopped.store(true, Ordering::Release);
                            audited.store(true, Ordering::Release);
                        }
                    },
                );
                marker.store(dispatched, Ordering::Release);
                std::future::pending::<()>().await;
            };
            let mut call = Box::pin(ProtectedCall::new(call, Some(permit)));
            tokio::select! {
                biased;
                _ = &mut call => panic!("模拟派发应保持 pending"),
                _ = tokio::task::yield_now() => {}
            }
            drop(call);
            assert_eq!(gate.is_blocked(), dispatched, "只有派发后的取消应封锁");
            assert_eq!(
                stopped.load(Ordering::Acquire),
                dispatched,
                "派发后必须同步停用"
            );
            assert_eq!(
                audited.load(Ordering::Acquire),
                dispatched,
                "派发后必须记录失败"
            );
            assert_eq!(
                gate.enter().await.is_err(),
                dispatched,
                "其他 backend 的结果必须一致"
            );
            leases.release_run("run-a");
            assert_eq!(
                leases.acquire_window("run-b", &window).is_err(),
                dispatched,
                "只保留派发后未知的窗口"
            );
        }
    }

    #[tokio::test]
    async fn legacy_partial_input_failure_blocks_other_writes() {
        let gate = RunWriteGate::default();
        let mut clicks = 0;
        assert!(
            legacy_input(&gate, || {
                clicks += 1;
                Ok::<(), ()>(())
            })
            .is_ok()
        );
        assert!(
            legacy_input(&gate, || {
                clicks += 1;
                Err::<(), _>(())
            })
            .is_err()
        );
        assert_eq!(clicks, 2, "双击第二次失败时第一次可能已经生效");
        assert!(
            gate.enter().await.is_err(),
            "部分点击或输入失败后不得切换 backend"
        );
        let fresh = RunWriteGate::default();
        assert!(!fresh.is_blocked(), "参数校验等派发前拒绝不应封锁");
    }

    #[test]
    fn legacy_panic_fails_closed_without_exposing_panic_payload() {
        let gate = RunWriteGate::default();
        let error = legacy_input::<(), &'static str>(&gate, || panic!("private-input"))
            .expect_err("输入 panic 不能报告成功");
        assert!(matches!(error, LegacyInputError::Panicked), "{error:?}");
        assert!(gate.is_blocked(), "panic 展开时 RAII 必须封锁本轮");
        assert!(
            !error.to_string().contains("private-input"),
            "不能把 panic 原文返回用户"
        );
    }

    #[test]
    fn chord_failure_or_panic_releases_attempted_modifiers_in_reverse() {
        for panic_on_press in [false, true] {
            let gate = RunWriteGate::default();
            let mut events = Vec::new();
            let result = legacy_chord(&gate, &[1, 2, 3], 4, |key, step| {
                events.push((key, step));
                if key == 3 && step == LegacyKeyStep::Press {
                    if panic_on_press {
                        panic!("private-input");
                    }
                    return Err("press failed");
                }
                Ok(())
            });
            assert!(result.is_err(), "修饰键中途失败不能报告成功");
            assert_eq!(
                events,
                vec![
                    (1, LegacyKeyStep::Press),
                    (2, LegacyKeyStep::Press),
                    (3, LegacyKeyStep::Press),
                    (3, LegacyKeyStep::Release),
                    (2, LegacyKeyStep::Release),
                    (1, LegacyKeyStep::Release),
                ],
                "失败后必须尽力倒序释放本次尝试按下的键"
            );
            assert!(gate.is_blocked(), "部分输入或 panic 后必须封锁本轮");
        }
    }

    #[test]
    fn chord_release_failure_still_attempts_remaining_releases() {
        let gate = RunWriteGate::default();
        let mut events = Vec::new();
        let result = legacy_chord(&gate, &[1, 2], 3, |key, step| {
            events.push((key, step));
            if key == 2 && step == LegacyKeyStep::Release {
                return Err("release failed");
            }
            Ok(())
        });
        assert!(
            result.is_err() && gate.is_blocked(),
            "释放失败必须报告未知并封锁"
        );
        assert_eq!(
            events.last(),
            Some(&(1, LegacyKeyStep::Release)),
            "释放失败也要继续释放其余键"
        );
    }

    #[test]
    fn browser_messages_distinguish_dispatched_and_possible_dispatch() {
        let verified = GatewayError::VerificationFailed {
            frame_id: "post".into(),
            evidence: "未成立".into(),
        };
        let known = browser_uncertain_message(&verified, false);
        assert!(
            known.contains("已派发") && !known.contains("可能已派发"),
            "{known}"
        );
        let unknown = GatewayError::Driver(DriverError::OutcomeUnknown("unknown".into()));
        let possible = browser_uncertain_message(&unknown, true);
        assert!(
            possible.contains("可能已派发") && possible.contains("禁止自动重试"),
            "{possible}"
        );
        let deadline = GatewayError::DriverDeadline {
            operation: "navigate",
            timeout: std::time::Duration::from_secs(1),
        };
        let gate = RunWriteGate::default();
        assert!(!stop_browser_if_uncertain(
            &deadline,
            &gate,
            false,
            || panic!("未派发不可取消")
        ));
        assert!(!gate.is_blocked(), "派发前超时不能封锁整个 run");
    }

    #[test]
    fn dispatch_marker_fences_even_an_unexpected_driver_error() {
        let error = GatewayError::Driver(DriverError::Failed("driver error".into()));
        let browser_gate = RunWriteGate::default();
        let browser_cancelled = AtomicBool::new(false);
        assert!(stop_browser_if_uncertain(
            &error,
            &browser_gate,
            true,
            || {
                browser_cancelled.store(true, Ordering::Release);
            }
        ));
        assert!(
            browser_gate.is_blocked() && browser_cancelled.load(Ordering::Acquire),
            "已派发后的任意错误都必须停用 browser"
        );
        let cua_gate = RunWriteGate::default();
        let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
        let window = format!("unexpected-driver-error-{}", std::process::id());
        leases.acquire_window("run-a", &window).expect("窗口租约");
        let cua_cancelled = AtomicBool::new(false);
        assert!(stop_cua_if_unconfirmed(
            &error,
            &cua_gate,
            &leases,
            "run-a",
            true,
            || {
                cua_cancelled.store(true, Ordering::Release);
            }
        ));
        assert!(
            cua_gate.is_blocked() && cua_cancelled.load(Ordering::Acquire),
            "已派发后的任意错误都必须停用 Cua"
        );
        leases.release_run("run-a");
        assert!(
            leases.acquire_window("run-b", &window).is_err(),
            "已派发后错误必须保留租约"
        );
    }

    #[test]
    fn pre_dispatch_rejections_do_not_quarantine_or_cancel() {
        let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
        let gate = RunWriteGate::default();
        let error = GatewayError::Protocol(ProtocolError::InvalidFrameTtl);
        let cancelled = AtomicBool::new(false);
        assert!(!stop_cua_if_unconfirmed(
            &error,
            &gate,
            &leases,
            "pre-run",
            false,
            || {
                cancelled.store(true, Ordering::Release);
            }
        ));
        assert!(!stop_browser_if_uncertain(&error, &gate, false, || {
            cancelled.store(true, Ordering::Release);
        }));
        assert!(!gate.is_blocked(), "派发前拒绝不能封锁整轮");
        assert!(
            !cancelled.load(Ordering::Acquire),
            "派发前拒绝不需要停用 driver"
        );
    }

    #[test]
    fn all_computer_write_entry_points_share_the_run_gate() {
        for tool in [
            "computer_act",
            "computer_browser_act",
            "click",
            "type_text",
            "key",
            "scroll",
        ] {
            assert!(is_write_tool(tool), "{tool} 可能写入，必须受同一轮次闸约束");
        }
        for tool in [
            "computer_list",
            "computer_observe",
            "computer_wait",
            "computer_browser_observe",
        ] {
            assert!(!is_write_tool(tool), "{tool} 只观察，不能被当作写入");
        }
    }
}
