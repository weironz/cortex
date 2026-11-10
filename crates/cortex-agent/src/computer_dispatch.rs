//! Gateway 授权完成后的持久派发边界。此模块不认证、不装配 SQLite。
//! 实际宿主必须从可信调用/审批上下文提供实现；模型 JSON 不能创建许可。

use std::sync::Arc;

/// 已由 Gateway 完成 policy、frame/app/ref 授权的实际目标。
///
/// 外部宿主可以实现存储，但不能自己签发“Gateway 已授权”的目标。
/// ```compile_fail
/// let _ = cortex_agent::computer_dispatch::AuthorizedDispatch::new(
///     "act", "app".into(), "window".into(), "frame".into());
/// ```
#[derive(Debug)]
pub struct AuthorizedDispatch {
    kind: &'static str,
    app: String,
    window: String,
    frame: String,
}

impl AuthorizedDispatch {
    pub(crate) fn new(kind: &'static str, app: String, window: String, frame: String) -> Self {
        Self {
            kind,
            app,
            window,
            frame,
        }
    }
    pub fn kind(&self) -> &'static str {
        self.kind
    }
    pub fn app(&self) -> &str {
        &self.app
    }
    pub fn window(&self) -> &str {
        &self.window
    }
    pub fn frame(&self) -> &str {
        &self.frame
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DispatchOutcome {
    Verified,
    Unknown,
}

struct ActivePermit {
    authority: Option<Arc<dyn DispatchSendAuthority>>,
}
tokio::task_local! { static ACTIVE_PERMIT: ActivePermit; }

/// 持有可信宿主的发送临界区；不能序列化成模型参数或传给其他进程。
pub struct DispatchSendGuard {
    _guard: Box<dyn Send>,
}
impl std::fmt::Debug for DispatchSendGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchSendGuard")
            .finish_non_exhaustive()
    }
}
impl DispatchSendGuard {
    /// 仅是可信宿主实现的适配入口，本身不校验租约或签发权限。
    /// 宿主必须先核对真实 lease/epoch，再传入覆盖实际发送的原 guard。
    pub fn from_host_guard(guard: impl Send + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

/// 唯一可信 Host 提供的输入代际检查；与 DurableDispatch 相同，不认证 Rust 调用者。
/// 此方法不得等待：退役正在占用临界区时拒绝，不能等待后复活旧票据。
pub trait DispatchSendAuthority: std::any::Any + Send + Sync + std::fmt::Debug {
    fn enter_send(&self) -> Result<DispatchSendGuard, String>;
}

/// 真正 pipe/CDP 写入前取得原宿主 guard，持有到发送完成。
/// 只有 Gateway 的布尔作用域或伪造参数不足以通过此检查。
pub fn enter_dispatch_send() -> Result<DispatchSendGuard, String> {
    let authority = current_send_authority()
        .ok_or_else(|| "电脑发送缺少可信宿主的租约/代际作用域".to_owned())?;
    authority.enter_send()
}

/// 供唯一宿主把原运行时句柄绑定到已有票据；返回值不是可序列化权限。
pub fn current_send_authority() -> Option<Arc<dyn DispatchSendAuthority>> {
    ACTIVE_PERMIT
        .try_with(|permit| permit.authority.clone())
        .ok()
        .flatten()
}

/// 只供真正驱动检查；写入作用域只能由持有 prepare 许可的 Gateway 建立。
///
/// ```compile_fail
/// let _ = cortex_agent::computer_dispatch::permit_scope(async {});
/// ```
pub fn has_dispatch_permit() -> bool {
    ACTIVE_PERMIT.try_with(|_| true).unwrap_or(false)
}

pub(crate) async fn permit_scope<F: std::future::Future>(
    authority: Option<Arc<dyn DispatchSendAuthority>>,
    future: F,
) -> F::Output {
    ACTIVE_PERMIT
        .scope(ActivePermit { authority }, future)
        .await
}

/// 只能消费一次；取消/丢弃必须保留未知 fence，不能靠 Drop 解锁。
#[async_trait::async_trait]
pub trait DispatchPermit: Send {
    /// 无 OS 输入的驱动可不提供；实际桌面/浏览器写入必须取得该宿主作用域。
    fn send_authority(&self) -> Option<Arc<dyn DispatchSendAuthority>> {
        None
    }
    async fn finish(self: Box<Self>, outcome: DispatchOutcome) -> Result<(), String>;
}

/// prepare 必须完成持久提交后才返回许可；所有阻塞存储及最终 Drop 属于实现责任。
#[async_trait::async_trait]
pub trait DurableDispatch: Send + Sync + std::fmt::Debug {
    async fn prepare(&self, target: AuthorizedDispatch) -> Result<Box<dyn DispatchPermit>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug)]
    struct HostAuthority(Arc<AtomicBool>);
    impl DispatchSendAuthority for HostAuthority {
        fn enter_send(&self) -> Result<DispatchSendGuard, String> {
            if !self.0.load(Ordering::Acquire) {
                return Err("宿主代际已经退役".into());
            }
            Ok(DispatchSendGuard::from_host_guard(Arc::clone(&self.0)))
        }
    }

    #[tokio::test]
    async fn protocol_scope_without_host_authority_cannot_authorize_os_send() {
        assert!(enter_dispatch_send().is_err(), "作用域外不能获取发送许可");
        permit_scope(None, async {
            assert!(has_dispatch_permit(), "协议层仍须识别成功 prepare 的作用域");
            assert!(
                enter_dispatch_send().is_err(),
                "协议标记不能冒充宿主的真实输入租约"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn host_authority_is_rechecked_at_each_actual_send() {
        let active = Arc::new(AtomicBool::new(true));
        let host: Arc<dyn DispatchSendAuthority> = Arc::new(HostAuthority(Arc::clone(&active)));
        permit_scope(Some(host), async {
            drop(enter_dispatch_send().expect("当前宿主允许发送"));
            active.store(false, Ordering::Release);
            assert!(
                enter_dispatch_send().is_err(),
                "已有 prepare 不能使退役后的晚发送通过"
            );
        })
        .await;
        assert!(
            enter_dispatch_send().is_err(),
            "退出 Gateway 作用域后不能复用授权"
        );
    }
}
