//! 进程级状态。**它没有数据库句柄，也不会有。**

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::remote::Remote;
use crate::runner::SandboxRunner;

#[derive(Clone)]
pub struct AgentState {
    inner: Arc<Inner>,
}

struct Inner {
    runner: Arc<dyn SandboxRunner>,
    /// 反代进容器用的客户端。与 [`Remote`] 里那个分开：这条打的是容器
    /// （同网段、短超时），那条打的是 cortexd（可能跨机房）。
    http: reqwest::Client,
    remote: Remote,
    /// **Cortex 自己的持久层**：会话、消息、附件、同步流水。
    ///
    /// 与 [`Remote`] 分工分明：那条打的是**记忆服务**（抽取与召回），
    /// 这个是自己的库。会话曾经也在那边，而后果是「记忆服务挂了就没有产品」
    /// —— 见 `cortex-store` 的 crate 文档。
    ///
    /// `None` = 这个部署还没给 `CORTEX_DATABASE_URL`。阶段一先让它站住，
    /// 一条路由都还没搬过来。
    store: Option<cortex_store::Store>,
    /// 每个作用域最后一次真的被用是什么时候。
    ///
    /// # 为什么这份表在这儿，而不是继续读 cortexd 的令牌注册表
    ///
    /// 回收器原先靠注册表的时间戳找空闲容器（`idle_scopes`）。拆进程之后
    /// 那张表在 cortexd，而**容器生命周期本来就该归拥有容器的那一方**：
    /// 让记忆服务替 agent 记「哪个容器闲了」，等于把一件纯粹的编排状态
    /// 塞进它的授权表，而那张表回答的是「谁能在我身上干什么」。
    ///
    /// 记的时机也更准：这里记的是**真的有请求进了那个容器**，而注册表那个
    /// 时间戳在「问了一次钥匙但容器没起来」时也会被刷新。
    last_use: Mutex<HashMap<String, Instant>>,
}

impl AgentState {
    #[must_use]
    pub fn new(
        runner: Arc<dyn SandboxRunner>,
        http: reqwest::Client,
        remote: Remote,
        store: Option<cortex_store::Store>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                runner,
                http,
                remote,
                store,
                last_use: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Cortex 自己的库。**现在还没有任何路由用它** —— 它先站住，
    /// 会话那一批路由分阶段搬过来（见 plan 的阶段三）。
    ///
    /// `None` = 这个部署还没给 `CORTEX_DATABASE_URL`，退回无状态形态。
    #[must_use]
    pub fn store(&self) -> Option<&cortex_store::Store> {
        self.inner.store.as_ref()
    }

    #[must_use]
    pub fn runner(&self) -> &Arc<dyn SandboxRunner> {
        &self.inner.runner
    }

    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    #[must_use]
    pub fn remote(&self) -> &Remote {
        &self.inner.remote
    }

    /// 记一次「这个作用域刚被用过」。
    pub fn touch(&self, scope_key: &str) {
        if let Ok(mut g) = self.inner.last_use.lock() {
            g.insert(scope_key.to_owned(), Instant::now());
        }
    }

    /// 闲置超过 `idle` 的作用域。回收器按它决定停谁。
    #[must_use]
    pub fn idle_scopes(&self, idle: Duration) -> Vec<String> {
        let now = Instant::now();
        self.inner.last_use.lock().map_or_else(
            |_| Vec::new(),
            |g| {
                g.iter()
                    .filter(|(_, at)| now.duration_since(**at) >= idle)
                    .map(|(k, _)| k.clone())
                    .collect()
            },
        )
    }

    /// 容器已经停了，别再把它算进空闲表。
    pub fn forget(&self, scope_key: &str) {
        if let Ok(mut g) = self.inner.last_use.lock() {
            g.remove(scope_key);
        }
    }

    /// 当前记着的全部作用域。
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        self.inner
            .last_use
            .lock()
            .map_or_else(|_| Vec::new(), |g| g.keys().cloned().collect())
    }
}
