//! 进程级电脑操作 lease。
//!
//! 一个 Cua session label 只描述生命周期，不能阻止另一轮 agent 同时观察并
//! 操作同一个顶层窗口。这里把“谁拥有这个窗口”放到进程级共享注册表：同一
//! 时间只允许一个 run 获得窗口 lease。C3 的多 agent 支持需要显式扩展成多
//! 窗口能力，而不是悄悄删掉这条排他约束。

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::collections::{HashMap, HashSet};
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::ffi::c_void;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::sync::{Arc, Mutex};

#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[derive(Debug)]
struct RunIncarnation {
    registry: Arc<()>,
    run_id: String,
    recovered: AtomicBool,
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[derive(Debug)]
struct WindowIncarnation {
    run: Arc<RunIncarnation>,
    original_handle: usize,
    closed: AtomicBool,
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[derive(Debug, Default)]
struct RecoveryEntries {
    runs: HashMap<String, Arc<RunIncarnation>>,
    windows: HashMap<String, Arc<WindowIncarnation>>,
}

/// 只从原 registry 已有窗口铸造；Clone 保留同一原对象，不允许用 run/PID/摘要重建。
#[cfg(any(windows, test))]
#[derive(Clone)]
pub(crate) struct WindowRecoveryBinding {
    registry: Arc<ComputerLeaseRegistry>,
    run_id: String,
    incarnation: Arc<RunIncarnation>,
    windows: Vec<(String, Arc<WindowIncarnation>)>,
}

#[cfg(any(windows, test))]
impl std::fmt::Debug for WindowRecoveryBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowRecoveryBinding")
            .field("run_id", &self.run_id)
            .field("windows", &self.windows.len())
            .finish_non_exhaustive()
    }
}

#[cfg(any(windows, test))]
impl PartialEq for WindowRecoveryBinding {
    fn eq(&self, other: &Self) -> bool {
        self.same_binding(other)
    }
}

#[cfg(any(windows, test))]
impl Eq for WindowRecoveryBinding {}

/// 所有本机 agent run 共享的窗口所有权表。
#[derive(Debug, Default)]
pub(crate) struct ComputerLeaseRegistry {
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    identity: Arc<()>,
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    windows: Mutex<HashMap<String, String>>,
    /// 输入状态或已派发动作的后置结果未确认时，run 不能靠重试取回窗口。
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    quarantined_runs: Mutex<HashSet<String>>,
    /// Windows 命名事件的原生句柄；对象存在期间占用窗口。
    ///
    /// `HashMap` 不能放进 `Mutex` 再同时存 `Send` 的原生句柄包装；直接保存
    /// 原生指针并只在本模块里 close，避免把生命周期语义扩散到调用方。
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    os_mutexes: Mutex<HashMap<String, AtomicPtr<c_void>>>,
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    recovery: Mutex<RecoveryEntries>,
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
impl Drop for ComputerLeaseRegistry {
    fn drop(&mut self) {
        if let Ok(mut mutexes) = self.os_mutexes.lock() {
            for handle in mutexes.values() {
                close_windows_mutex(handle.load(Ordering::Acquire));
            }
            mutexes.clear();
        }
    }
}

impl ComputerLeaseRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 获取窗口 lease；本轮持有过的窗口直到清理完成后一起释放。
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn acquire_window(&self, run_id: &str, window_id: &str) -> Result<(), String> {
        let mut windows = self
            .windows
            .lock()
            .map_err(|_| "电脑操作 lease 表已损坏".to_owned())?;
        let mut os_mutexes = self
            .os_mutexes
            .lock()
            .map_err(|_| "电脑操作 lease 表已损坏".to_owned())?;
        let quarantined = self
            .quarantined_runs
            .lock()
            .map_err(|_| "电脑操作隔离表已损坏".to_owned())?;
        let mut recovery = self
            .recovery
            .lock()
            .map_err(|_| "电脑操作原窗口保管表已损坏".to_owned())?;
        if quarantined.contains(run_id) {
            return Err(
                "上一轮电脑操作的输入状态未知或已派发动作的后置结果未确认；请先检查键鼠与目标窗口，再重启本机代理".to_owned(),
            );
        }

        if let Some(owner) = windows.get(window_id) {
            if quarantined.contains(owner) {
                return Err(
                    "上一轮电脑操作的输入状态未知或已派发动作的后置结果未确认；请先检查键鼠与目标窗口，再重启本机代理"
                        .to_owned(),
                );
            }
            if owner != run_id {
                return Err(format!(
                    "窗口 {window_id} 正被另一轮电脑操作占用；等它停止或交还控制权后再试"
                ));
            }
            let Some(handle) = os_mutexes.get(window_id) else {
                return Err(format!(
                    "窗口 {window_id} 的进程内 lease 存在，但跨进程锁句柄丢失"
                ));
            };
            let entry = recovery
                .windows
                .get(window_id)
                .ok_or("原窗口条目保管已遗失")?;
            let run = recovery
                .runs
                .get(run_id)
                .ok_or("原 run incarnation 已遗失")?;
            if run.run_id != run_id
                || !Arc::ptr_eq(&run.registry, &self.identity)
                || run.recovered.load(Ordering::Acquire)
                || !Arc::ptr_eq(&entry.run, run)
                || entry.closed.load(Ordering::Acquire)
                || entry.original_handle != handle.load(Ordering::Acquire) as usize
            {
                return Err("原窗口条目或事件句柄已变化".into());
            }
            // 同 run 的每个 observation 都会重申同一个窗口 lease。必须复用
            // 已持有的锁句柄；再次创建同名对象会打开一个新句柄，随后
            // 覆盖旧句柄，造成每个 observe 都泄漏一个 Mutant。
            return Ok(());
        }

        // Windows 命名事件的创建结果提供跨进程排他；已存在则立即失败。
        if os_mutexes.contains_key(window_id) || recovery.windows.contains_key(window_id) {
            return Err("窗口记录不完整，不能覆盖既有句柄保管".into());
        }
        let os_handle = acquire_windows_mutex(window_id)?;
        let run = Arc::clone(recovery.runs.entry(run_id.to_owned()).or_insert_with(|| {
            Arc::new(RunIncarnation {
                registry: Arc::clone(&self.identity),
                run_id: run_id.to_owned(),
                recovered: AtomicBool::new(false),
            })
        }));
        let entry = Arc::new(WindowIncarnation {
            run,
            original_handle: os_handle.load(Ordering::Acquire) as usize,
            closed: AtomicBool::new(false),
        });
        // 切到另一个窗口不代表旧窗口上已经派发的输入安全结束；
        // 本轮的工具调用预算也给持有窗口数提供了上界。
        windows.insert(window_id.to_owned(), run_id.to_owned());
        os_mutexes.insert(window_id.to_owned(), os_handle);
        recovery.windows.insert(window_id.to_owned(), entry);
        Ok(())
    }

    /// 不释放输入状态或已派发动作后置结果未确认的窗口；需要用户检查后重启代理解除。
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn quarantine_run(&self, run_id: &str) {
        if let Ok(mut runs) = self.quarantined_runs.lock() {
            runs.insert(run_id.to_owned());
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn is_quarantined(&self, run_id: &str) -> bool {
        self.quarantined_runs
            .lock()
            .map_or(true, |runs| runs.contains(run_id))
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn release_window(&self, run_id: &str, window_id: &str) {
        let Ok(mut windows) = self.windows.lock() else {
            return;
        };
        let Ok(mut os_mutexes) = self.os_mutexes.lock() else {
            return;
        };
        let Ok(mut quarantined) = self.quarantined_runs.lock() else {
            return;
        };
        let Ok(mut recovery) = self.recovery.lock() else {
            return;
        };
        if quarantined.contains(run_id) {
            return;
        }
        if windows.get(window_id).is_some_and(|owner| owner == run_id) {
            let Some(entry) = recovery.windows.get(window_id) else {
                return;
            };
            let Some(handle) = os_mutexes.get(window_id) else {
                return;
            };
            if close_original_event(entry, handle).is_err() {
                quarantined.insert(run_id.to_owned());
                return;
            }
            windows.remove(window_id);
            os_mutexes.remove(window_id);
            recovery.windows.remove(window_id);
            if !windows.values().any(|owner| owner == run_id) {
                recovery.runs.remove(run_id);
            }
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn release_run(&self, run_id: &str) {
        let _ = self.try_release_run(run_id);
    }

    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn try_release_run(&self, run_id: &str) -> bool {
        let Ok(mut windows) = self.windows.lock() else {
            return false;
        };
        let Ok(mut os_mutexes) = self.os_mutexes.lock() else {
            return false;
        };
        let Ok(mut quarantined) = self.quarantined_runs.lock() else {
            return false;
        };
        let Ok(mut recovery) = self.recovery.lock() else {
            return false;
        };
        if quarantined.contains(run_id) {
            return false;
        }
        let released: Vec<String> = windows
            .iter()
            .filter(|(_, owner)| owner.as_str() == run_id)
            .map(|(window, _)| window.clone())
            .collect();
        for window in &released {
            let (Some(entry), Some(handle)) =
                (recovery.windows.get(window), os_mutexes.get(window))
            else {
                return false;
            };
            if close_original_event(entry, handle).is_err() {
                quarantined.insert(run_id.to_owned());
                return false;
            }
        }
        for window in released {
            windows.remove(&window);
            os_mutexes.remove(&window);
            recovery.windows.remove(&window);
        }
        recovery.runs.remove(run_id);
        true
    }

    /// 必须先实际取得窗口；仅有 run 字符串或命名事件名不足以铸造恢复绑定。
    #[cfg(any(windows, test))]
    pub(crate) fn bind_recovery(
        self: &Arc<Self>,
        run_id: &str,
    ) -> Result<WindowRecoveryBinding, String> {
        let windows = self.windows.lock().map_err(|_| "窗口 lease 表失效")?;
        let handles = self.os_mutexes.lock().map_err(|_| "窗口事件保管表失效")?;
        let _quarantined = self.quarantined_runs.lock().map_err(|_| "窗口隔离表失效")?;
        let recovery = self.recovery.lock().map_err(|_| "原窗口保管表失效")?;
        let incarnation = recovery
            .runs
            .get(run_id)
            .ok_or("该 run 没有已取得的原窗口")?;
        let mut held = Vec::new();
        for (window, owner) in windows.iter().filter(|(_, owner)| owner.as_str() == run_id) {
            let entry = recovery.windows.get(window).ok_or("原窗口条目已遗失")?;
            let handle = handles.get(window).ok_or("原窗口事件句柄已遗失")?;
            if owner != &incarnation.run_id
                || !Arc::ptr_eq(&entry.run, incarnation)
                || entry.closed.load(Ordering::Acquire)
                || entry.original_handle != handle.load(Ordering::Acquire) as usize
            {
                return Err("原窗口/run incarnation 或事件句柄不匹配".into());
            }
            held.push((window.clone(), Arc::clone(entry)));
        }
        if held.is_empty() || incarnation.recovered.load(Ordering::Acquire) {
            return Err("没有可保管的原窗口，不能重建恢复能力".into());
        }
        held.sort_by(|first, second| first.0.cmp(&second.0));
        Ok(WindowRecoveryBinding {
            registry: Arc::clone(self),
            run_id: run_id.to_owned(),
            incarnation: Arc::clone(incarnation),
            windows: held,
        })
    }

    #[cfg(test)]
    fn owner_of(&self, window_id: &str) -> Option<String> {
        self.windows
            .lock()
            .ok()
            .and_then(|windows| windows.get(window_id).cloned())
    }
}

#[cfg(any(windows, test))]
impl WindowRecoveryBinding {
    /// 只比较原对象身份；不能替代真实静止证明或已提交的 Core 恢复证明。
    pub(crate) fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.registry, &other.registry)
            && self.run_id == other.run_id
            && Arc::ptr_eq(&self.incarnation, &other.incarnation)
            && self.windows.len() == other.windows.len()
            && self
                .windows
                .iter()
                .zip(&other.windows)
                .all(|(first, second)| first.0 == second.0 && Arc::ptr_eq(&first.1, &second.1))
    }

    /// 只由 Host 在 Core committed、原 Supervisor 仍 Retiring/保管租约时调用。
    /// 释放成功后仍须由主线消费 Drained.activate；本函数从不开放新输入代际。
    #[cfg(windows)]
    pub(crate) fn release(
        &self,
        candidate: &crate::computer_input::RecoveryCandidate,
        drained: &crate::computer_input::DrainedInputEpoch,
        committed: &crate::computer_ledger::CommittedInputRecovery,
    ) -> Result<(), String> {
        if !candidate
            .operation()
            .window_recovery
            .as_ref()
            .is_some_and(|original| self.same_binding(original))
        {
            return Err("窗口保管不属于该候选的原 registry/run/incarnation".into());
        }
        // 摘要只关联私有构造的 committed 证明；静止必须核对实际原对象。
        drained.validate_candidate(candidate)?;
        if !committed.matches(candidate.binding_digest())
            || candidate.epoch().as_u64().checked_add(1) != Some(committed.epoch().as_u64())
        {
            return Err("窗口恢复缺少匹配原绑定及相邻代际的 Core committed 证明".into());
        }
        self.release_original_entries()
    }

    fn release_original_entries(&self) -> Result<(), String> {
        if self.windows.is_empty()
            || self.run_id != self.incarnation.run_id
            || !Arc::ptr_eq(&self.registry.identity, &self.incarnation.registry)
        {
            return Err("原 registry/run incarnation 不匹配，不能释放窗口".into());
        }
        // 与 acquire/release 完全一致的锁序；没有在窗口锁内等待 SQL 或 Job。
        let mut windows = self
            .registry
            .windows
            .lock()
            .map_err(|_| "窗口 lease 表失效")?;
        let mut handles = self
            .registry
            .os_mutexes
            .lock()
            .map_err(|_| "窗口事件表失效")?;
        let mut quarantined = self
            .registry
            .quarantined_runs
            .lock()
            .map_err(|_| "窗口隔离表失效")?;
        let mut recovery = self
            .registry
            .recovery
            .lock()
            .map_err(|_| "原窗口保管表失效")?;
        if recovery
            .runs
            .get(&self.run_id)
            .is_some_and(|current| !Arc::ptr_eq(current, &self.incarnation))
        {
            return Err("该 run 名称已属于另一 incarnation，不能释放其窗口".into());
        }
        // 无实际 dispatch 的失败也可能留下 Core Unknown。无需预先隔离，但必须
        // 在关闭任何原事件前永久撤销旧 run，防止迟到 observe/callback 重新抢窗。
        quarantined.insert(self.run_id.clone());
        if self.incarnation.recovered.load(Ordering::Acquire) {
            return if windows.values().any(|owner| owner == &self.run_id) {
                Err("已释放的旧 run 出现额外窗口条目，继续拒绝".into())
            } else {
                Ok(())
            };
        }
        for (window, _) in windows.iter().filter(|(_, owner)| *owner == &self.run_id) {
            let entry = recovery.windows.get(window).ok_or("原窗口条目已遗失")?;
            if !self
                .windows
                .iter()
                .any(|(bound, original)| bound == window && Arc::ptr_eq(original, entry))
            {
                return Err("旧 run 存在未绑定的窗口条目，不能部分宣称恢复成功".into());
            }
        }
        let mut retained = Vec::new();
        for (window, original) in &self.windows {
            if windows.get(window) == Some(&self.run_id)
                && recovery
                    .windows
                    .get(window)
                    .is_some_and(|current| Arc::ptr_eq(current, original))
            {
                let handle = handles.get(window).ok_or("原窗口事件句柄已遗失")?;
                let current = handle.load(Ordering::Acquire);
                if !Arc::ptr_eq(&original.run, &self.incarnation)
                    || (original.closed.load(Ordering::Acquire) && !current.is_null())
                    || (!original.closed.load(Ordering::Acquire)
                        && original.original_handle != current as usize)
                {
                    return Err("原窗口事件已被替换，不能按名称释放".into());
                }
                retained.push(window.clone());
            } else if !original.closed.load(Ordering::Acquire) {
                return Err("尚未关闭的原窗口条目已变化，不能释放其他 owner".into());
            }
        }
        // CloseHandle 失败保留全部条目及旧 run 隔离；已关闭的原事件单独记账，
        // 重试不会双重关闭，也不会误关复用了同一数值的新句柄。
        for window in &retained {
            let entry = recovery.windows.get(window).ok_or("原窗口条目已遗失")?;
            let handle = handles.get(window).ok_or("原窗口事件句柄已遗失")?;
            close_original_event(entry, handle)?;
        }
        for window in retained {
            windows.remove(&window);
            handles.remove(&window);
            recovery.windows.remove(&window);
        }
        recovery.runs.remove(&self.run_id);
        self.incarnation.recovered.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn close_original_event(
    entry: &WindowIncarnation,
    handle: &AtomicPtr<c_void>,
) -> Result<(), String> {
    let current = handle.load(Ordering::Acquire);
    if entry.closed.load(Ordering::Acquire) {
        return if current.is_null() {
            Ok(())
        } else {
            Err("已关闭的原条目包含另一事件句柄".into())
        };
    }
    if current as usize != entry.original_handle {
        return Err("实际事件句柄不属于原窗口条目".into());
    }
    #[cfg(windows)]
    {
        // SAFETY: 句柄由本 registry 创建；全部窗口生命周期锁仍持有，尚未关闭。
        if current.is_null() || unsafe { windows_sys::Win32::Foundation::CloseHandle(current) } == 0
        {
            return Err(format!(
                "原窗口事件关闭失败，继续隔离：{}",
                std::io::Error::last_os_error()
            ));
        }
    }
    handle.store(std::ptr::null_mut(), Ordering::Release);
    entry.closed.store(true, Ordering::Release);
    Ok(())
}

#[cfg(windows)]
fn acquire_windows_mutex(window_id: &str) -> Result<AtomicPtr<c_void>, String> {
    use sha2::{Digest, Sha256};
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateEventW;

    // 使用 Global 命名空间，避免不同 session 出现
    // 同名语义。窗口 id 进入 hash，不直接作为对象名：对象名有长度和字符
    // 限制，原始 id 是攻击者可见的。
    let digest = Sha256::digest(window_id.as_bytes());
    let name = format!("Global\\cortex-computer-window-{:x}\0", digest);
    let wide: Vec<u16> = name.encode_utf16().collect();
    // 命名事件以对象是否已存在决定独占，而不是等待线程所有权：异步任务
    // 可在其他线程关闭句柄，进程退出时内核也会回收它。
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    let status = unsafe { GetLastError() };
    if handle.is_null() {
        return Err(format!(
            "无法创建窗口互斥对象：{}",
            std::io::Error::last_os_error()
        ));
    }
    if status == ERROR_ALREADY_EXISTS {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err("窗口正被另一个 Cortex 进程占用".to_owned());
    }
    Ok(AtomicPtr::new(handle))
}

#[cfg(windows)]
fn close_windows_mutex(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    }
}

#[cfg(all(not(windows), any(target_os = "macos", test)))]
fn acquire_windows_mutex(_window_id: &str) -> Result<AtomicPtr<c_void>, String> {
    Ok(AtomicPtr::new(std::ptr::null_mut()))
}

#[cfg(test)]
#[path = "computer_lease_recovery_test.rs"]
mod recovery_tests;

#[cfg(all(test, windows))]
mod windows_mutex_tests {
    use super::*;

    /// 获取后再次获取必须失败。named mutex 的全局命名空间是跨进程边界，
    /// 进程内表必须与之保持同一语义，不能把 second acquire 放进 stale 窗口。
    #[test]
    fn held_named_mutex_blocks_a_second_acquire() {
        let window_id = format!(
            "cross-process-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        );
        let registry = ComputerLeaseRegistry::new();
        registry
            .acquire_window("run-a", &window_id)
            .expect("first registry acquire owns the OS mutex");
        registry
            .acquire_window("run-b", &window_id)
            .expect_err("held OS mutex must block the registry acquire");
        registry.release_run("run-a");
    }

    #[test]
    fn different_registry_cannot_reenter_on_same_thread_and_can_acquire_after_cross_thread_release()
    {
        let window_id = format!(
            "cross-thread-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let first = std::sync::Arc::new(ComputerLeaseRegistry::new());
        let second = ComputerLeaseRegistry::new();
        first
            .acquire_window("a", &window_id)
            .expect("首个 run 应取得窗口");
        assert!(
            second.acquire_window("b", &window_id).is_err(),
            "另一个注册表即使在同一线程也不能重入"
        );
        let released = std::sync::Arc::clone(&first);
        std::thread::spawn(move || released.release_run("a"))
            .join()
            .expect("其他线程可释放 lease");
        second
            .acquire_window("b", &window_id)
            .expect("跨线程释放后必须能重新获取");
        second.release_run("b");
    }

    #[test]
    fn named_lease_excludes_another_process_until_release() {
        const WINDOW_ENV: &str = "CORTEX_LEASE_TEST_WINDOW";
        const EXPECT_ENV: &str = "CORTEX_LEASE_TEST_EXPECT";
        if let Ok(window) = std::env::var(WINDOW_ENV) {
            let leases = ComputerLeaseRegistry::new();
            let acquired = leases.acquire_window("child", &window).is_ok();
            assert_eq!(
                acquired,
                std::env::var(EXPECT_ENV).as_deref() == Ok("allowed"),
                "子进程锁状态不符合预期"
            );
            return;
        }
        let window = format!(
            "cross-process-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟")
                .as_nanos()
        );
        let leases = ComputerLeaseRegistry::new();
        leases
            .acquire_window("parent", &window)
            .expect("父进程应先取得锁");
        let child = |expected: &str| {
            let module = module_path!()
                .split_once("::")
                .map_or(module_path!(), |(_, tail)| tail);
            let name = format!("{module}::named_lease_excludes_another_process_until_release");
            std::process::Command::new(std::env::current_exe().expect("测试程序路径"))
                .args(["--exact", &name, "--nocapture"])
                .env(WINDOW_ENV, &window)
                .env(EXPECT_ENV, expected)
                .output()
                .expect("子进程启动")
        };
        let blocked = child("blocked");
        assert!(
            blocked.status.success()
                && String::from_utf8_lossy(&blocked.stdout).contains("running 1 test"),
            "持锁期间子进程必须被拒绝：{}",
            String::from_utf8_lossy(&blocked.stdout)
        );
        leases.release_run("parent");
        let allowed = child("allowed");
        assert!(
            allowed.status.success()
                && String::from_utf8_lossy(&allowed.stdout).contains("running 1 test"),
            "释放后子进程必须取得锁：{}",
            String::from_utf8_lossy(&allowed.stdout)
        );
    }
}

#[cfg(all(not(windows), any(target_os = "macos", test)))]
fn close_windows_mutex(_handle: *mut c_void) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantined_run_cannot_release_window_or_reacquire() {
        let leases = ComputerLeaseRegistry::new();
        leases
            .acquire_window("run-a", "w-quarantine")
            .expect("先获得租约");
        leases.quarantine_run("run-a");
        leases.release_window("run-a", "w-quarantine");
        assert!(!leases.try_release_run("run-a"), "隔离期间不能释放全部租约");
        assert!(
            leases.acquire_window("run-b", "w-quarantine").is_err(),
            "另一轮不能占用隔离窗口"
        );
        assert!(
            leases.acquire_window("run-a", "other-window").is_err(),
            "隔离轮次不能重新取租约"
        );
    }

    /// Named mutex 是跨进程的；两次“释放后重取”必须真的关闭了句柄，
    /// 否则同名窗口在同一个进程里会被残留所有权挡住。
    #[test]
    fn switching_windows_keeps_both_leases_until_release() {
        let leases = ComputerLeaseRegistry::new();
        leases.acquire_window("run-a", "w1").expect("first lease");
        let error = leases
            .acquire_window("run-b", "w1")
            .expect_err("two runs must not drive the same window");
        assert!(error.contains("另一轮"), "{error}");

        leases
            .acquire_window("run-a", "w2")
            .expect("same run switches");
        assert_eq!(leases.owner_of("w1").as_deref(), Some("run-a"));
        assert_eq!(leases.owner_of("w2").as_deref(), Some("run-a"));

        assert!(
            leases.acquire_window("run-b", "w1").is_err(),
            "切换到新窗口不能提前交还旧窗口"
        );
        leases.release_run("run-a");
        leases
            .acquire_window("run-b", "w1")
            .expect("run-a 完整结束后 w1 才释放");
        assert_eq!(leases.owner_of("w2"), None);
        assert_eq!(leases.owner_of("w1").as_deref(), Some("run-b"));
    }

    #[test]
    fn closing_a_run_releases_every_window_it_owned() {
        let leases = ComputerLeaseRegistry::new();
        let window = format!(
            "release-run-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        );
        leases.acquire_window("run-a", &window).expect("lease");
        leases.release_run("run-a");
        assert_eq!(leases.owner_of(&window), None);
        leases
            .acquire_window("run-b", &window)
            .expect("清理成功后另一轮应能取得同一窗口");
        leases.release_run("run-b");
    }

    #[test]
    fn os_mutex_is_tracked_and_removed_with_the_window() {
        let leases = ComputerLeaseRegistry::new();
        leases
            .acquire_window("run-a", "unique-window-for-mutex-test")
            .expect("lease");
        {
            let mutexes = leases.os_mutexes.lock().expect("mutex map");
            assert!(
                mutexes.contains_key("unique-window-for-mutex-test"),
                "Windows lease 必须登记跨进程 mutex；否则 release 时无法关闭句柄"
            );
        }
        leases.release_run("run-a");
        let mutexes = leases.os_mutexes.lock().expect("mutex map");
        assert!(
            !mutexes.contains_key("unique-window-for-mutex-test"),
            "释放窗口时必须同时释放跨进程 mutex，否则同窗口永久不可用"
        );
    }

    #[test]
    fn repeated_observation_reuses_the_existing_window_mutex() {
        let leases = ComputerLeaseRegistry::new();
        let window = "repeat-observation-window";
        leases.acquire_window("run-a", window).expect("first lease");
        let first = {
            let mutexes = leases.os_mutexes.lock().expect("mutex map");
            mutexes.get(window).expect("mutex").load(Ordering::Acquire)
        };

        for _ in 0..64 {
            leases
                .acquire_window("run-a", window)
                .expect("same run re-observation");
        }

        let mutexes = leases.os_mutexes.lock().expect("mutex map");
        assert_eq!(
            mutexes.len(),
            1,
            "重复 observation 不能为同一个窗口新增 mutex 句柄"
        );
        assert_eq!(
            mutexes.get(window).expect("mutex").load(Ordering::Acquire),
            first,
            "重复 observation 必须复用已经持有的句柄"
        );
    }
}
