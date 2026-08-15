//! 进程级状态。
//!
//! # 它现在有数据库句柄了
//!
//! 这份文件的第一行以前写着「它没有数据库句柄，也不会有」。那句话是对的
//! ——**在那时**：那时这个进程只管起容器，会话与身份都住在记忆服务里。
//!
//! 2026-08-15 把边界重划了：会话不是记忆能力，身份是 agent 产品的身份。
//! 判据只有一句 ——「这张表离开记忆能力还有没有意义」。沙箱快照与自带
//! API key 是决定性证据：它们跟记忆毫无关系，在那边只因为「那边有库」。
//!
//! 于是这里有了两个句柄，分工必须分明：
//!
//! - [`Inner::store`] —— **Cortex 自己的库**。会话、消息、附件、同步流水。
//! - [`Inner::remote`] —— **记忆服务**（Cormex）。抽取与召回，只有 HTTP。
//!
//! 混用它们的后果就是当初那个 bug 本身。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::{AuthMode, TicketBook};
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
    /// `None` = 这个部署还没给 `CORTEX_DATABASE_URL`。此时账号那一批端点
    /// 全部报 501 —— 假装能登录比明说不支持糟得多。
    store: Option<cortex_store::Store>,
    /// 账号那几张表的连接池（`cortex_auth`），以及租户池注册表。
    ///
    /// **与租户库的池分开**：`cortex-store` 的每条查询都假设自己在某个
    /// 租户的 schema 里，而账号是跨租户的。混用一个池意味着 `search_path`
    /// 要来回改，而那正是 [`cortex_store::TenantPools`] 的文档里说
    /// 「漏一次就是跨用户泄露」的那种改法。
    ///
    /// `None` 与 `store` 同进同退：没给数据库地址就两个都没有。
    accounts: Option<Accounts>,
    /// 这个部署要不要凭据，以及那把预共享 token 的摘要。
    auth: AuthMode,
    /// 短命票据。给加不了请求头的连接用（`?ticket=`）。
    tickets: Arc<TicketBook>,
    /// 已签发的 access token。见 [`crate::accounts::AccessBook`]。
    access: Arc<crate::accounts::AccessBook>,
    //
    // 这里**还没有**委托令牌那本簿子（沙箱容器回调用的那种）。
    //
    // 签一把委托凭据要先读会话行（它绑的是「哪个会话、哪个项目、跑在
    // 云上还是本机」），而会话此刻还在记忆服务的库里。现在把那本簿子搬
    // 过来，这个进程能签出的只会是一把作用域全错的钥匙 —— 于是签发仍然
    // 由记忆服务做（`remote::Remote::delegate`），这边一把都认不出来。
    //
    // 它与 `/episodes`、`/delegated-tokens` 一起来（持久层阶段三）。
    // 那时 `auth::require` 里也要补回对应的那一支。
    //
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

/// 账号相关的两个句柄。
#[derive(Clone)]
pub struct Accounts {
    /// 直连 `cortex_auth` 的池
    pub pool: sqlx::PgPool,
    /// 按用户取租户库
    pub tenants: Arc<cortex_store::TenantPools>,
}

impl Accounts {
    /// 连 `cortex_auth`，并把全局那套 migration 跑上。
    ///
    /// 池子只要两条连接：账号相关的查询都是「登录时一次、刷新时一次」，
    /// 而真正的并发压力走的是租户池。
    ///
    /// # Errors
    /// 连不上，或者全局 migration 跑不过。
    pub async fn connect(database_url: &str) -> cortex_core::Result<Self> {
        cortex_store::Store::migrate_global(database_url)
            .await
            .map_err(|e| cortex_core::CortexError::Store(format!("全局 schema 迁移失败：{e}")))?;

        let options = std::str::FromStr::from_str(database_url)
            .map(|o: sqlx::postgres::PgConnectOptions| {
                // 账号表在 cortex_auth 里，而这个池**只**碰它们。
                // 不带 public 的话 `ulid` / `sha256` 两个 DOMAIN 解析不到
                o.options([("search_path", "cortex_auth,public")])
            })
            .map_err(|e: sqlx::Error| {
                cortex_core::CortexError::Config(format!("DATABASE_URL 不合法：{e}"))
            })?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .map_err(|e| cortex_core::CortexError::Store(format!("连 cortex_auth 失败：{e}")))?;

        Ok(Self {
            pool,
            tenants: Arc::new(cortex_store::TenantPools::new(database_url)),
        })
    }
}

impl AgentState {
    #[must_use]
    pub fn new(
        runner: Arc<dyn SandboxRunner>,
        http: reqwest::Client,
        remote: Remote,
        store: Option<cortex_store::Store>,
        accounts: Option<Accounts>,
        auth: AuthMode,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                runner,
                http,
                remote,
                store,
                accounts,
                auth,
                tickets: Arc::new(TicketBook::default()),
                access: Arc::new(crate::accounts::AccessBook::default()),
                last_use: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Cortex 自己的库，连的是 `public`。
    ///
    /// **多租户部署下不要直接用它做数据查询** —— 那会让所有人读写同一片库。
    /// 请求路径上一律先 [`AgentState::tenant`] 拿到本次请求所属的租户，
    /// 那个类型的存在就是为了让「漏一步」编译不过。
    ///
    /// `None` = 这个部署还没给 `CORTEX_DATABASE_URL`。
    #[must_use]
    pub fn store(&self) -> Option<&cortex_store::Store> {
        self.inner.store.as_ref()
    }

    /// 账号相关的两个句柄。
    ///
    /// # Errors
    /// 这个部署没接数据库。回 501 而不是 panic —— 「调用方会先检查」
    /// 不是一种保护，一次匿名 `GET /auth/me` 就能证伪它。
    pub fn accounts(&self) -> Result<&Accounts, crate::error::ApiError> {
        self.inner.accounts.as_ref().ok_or_else(|| {
            crate::error::ApiError::unsupported(
                "这个部署没有接数据库（CORTEX_DATABASE_URL 为空），账号功能不可用",
            )
        })
    }

    /// 这个部署的认证形态。
    #[must_use]
    pub fn auth_mode(&self) -> &AuthMode {
        &self.inner.auth
    }

    /// 短命票据簿。
    #[must_use]
    pub fn ticket_book(&self) -> &TicketBook {
        &self.inner.tickets
    }

    /// 已签发的 access token 簿子。
    #[must_use]
    pub fn access_book(&self) -> &crate::accounts::AccessBook {
        &self.inner.access
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
