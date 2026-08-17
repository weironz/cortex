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
//!
//! 混用它们的后果就是当初那个 bug 本身。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::{AuthMode, TicketBook};
use crate::runner::SandboxRunner;

/// 这个进程写会话 / 项目事件时盖的那个章（`device_id`）。
///
/// # 为什么是常量，而不是一个 `CORTEX_DEVICE_ID`
///
/// 这一列记的**不是「用户那台设备」** —— 链路上根本没有客户端设备身份
/// （见 `cortex_store::SessionRuntime` 的文档）。一次 PATCH 只有一个 HTTP
/// 请求，没有任何一处能证明它来自哪台机器，能诚实说出的只有「这条事件是
/// 编排器写的」。
///
/// 给它一个环境变量，等于放一个**配错了也没人看得出来**的旋钮：运维会以为
/// 自己在标注设备，而所有租户的所有会话事件仍然只由这一个进程写入。
pub const DEVICE_ID: &str = "cortex-agentd";

#[derive(Clone)]
pub struct AgentState {
    inner: Arc<Inner>,
}

struct Inner {
    runner: Arc<dyn SandboxRunner>,
    /// 反代进容器用的客户端：同网段、短超时。
    http: reqwest::Client,
    /// **Cortex 自己的持久层**：会话、消息、附件、同步流水。
    ///
    /// 这个进程曾经还持有一个打记忆服务的客户端。2026-08-17 连同长期记忆
    /// 一起去掉了 —— 会话早就搬进了这个库，而剩下那条抽取/召回的路在生产上
    /// 每轮被回 401，见 `cortex-store` 的 crate 文档与 `episodes::write_one`。
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
    /// 服务端那把 LLM key 组装出来的客户端。`POST /llm/stream` 用它。
    ///
    /// # 为什么它是 `Option` 而不是必填
    ///
    /// 这个进程的本职是编排容器，那件事一行模型配置都不需要。要求必填的
    /// 直接后果是：一台只跑沙箱、模型走别处的部署**起不来**，而报错说的是
    /// 「缺少 DEEPSEEK_API_KEY」—— 一句听起来像配置漏了、实际上是我们多要了
    /// 一样东西的话。
    ///
    /// `None` 时 `/llm/stream` 回 501（「这个部署永远提供不了」），
    /// 与 [`Self::accounts`] 同一个形状：**明说不支持，而不是假装能用**。
    llm: Option<cortex_llm::LlmClient>,
    /// 附件的字节住在哪儿（`/blobs` 那几条路要它）。
    ///
    /// `None` = 这个部署既没配 S3、也没显式给本地目录。此时 blob 端点一律
    /// 501 —— **上传成功却没有字节落地是「自证清白的坏数据」**：`blobs` 行
    /// 在、同步也推了，只有取回时才发现内容不存在，而那时已经追不回来源了。
    ///
    /// 与 [`Self::store`] 分工分明：那个是**指向字节的那一行**，这个是字节
    /// 本身。两者必须同时在，缺哪一半都会留下取不回内容的悬空引用。
    blobs: Option<crate::blobs::MediaStore>,
    /// 这个部署要不要凭据，以及那把预共享 token 的摘要。
    auth: AuthMode,
    /// 短命票据。给加不了请求头的连接用（`?ticket=`）。
    tickets: Arc<TicketBook>,
    /// 已签发的 access token。见 [`crate::accounts::AccessBook`]。
    access: Arc<crate::accounts::AccessBook>,
    /// 认证端点（login / refresh / register）的限流表。
    ///
    /// 与 [`Inner::tickets`] / [`Inner::access`] 同款的进程内簿子。它挂在
    /// 这儿而不是做成一层中间件，是因为键要从**请求体**里来（refresh token
    /// 的摘要、提交的用户名），而中间件那层还没解析请求体。
    /// 阈值与逐端点的键怎么选，见 [`crate::rate_limit`] 的模块文档。
    auth_throttle: Arc<crate::rate_limit::AuthThrottle>,
    /// 每个租户一条的实时推送总线。懒建，见 [`crate::sync_bus::SyncBuses`]。
    sync_buses: Arc<crate::sync_bus::SyncBuses>,
    /// 委托令牌那本簿子（沙箱容器回调用的那种）。见
    /// [`crate::delegated_token`]。
    ///
    /// # 它为什么终于能在这儿
    ///
    /// 这一段以前写着「这里**还没有**这本簿子」，理由是：签一把委托凭据要先
    /// 读会话行（它绑的是「哪个会话、哪个项目、跑在云上还是本机」），而会话
    /// 那时还在记忆服务的库里 —— 在这儿签只会签出一把作用域全错的钥匙。
    ///
    /// 2026-08-16 会话搬进了 [`Self::store`]，那个前提反转了：**签发方必须
    /// 是持有会话行的那一方**。于是簿子跟着会话走，[`crate::auth::require`]
    /// 里也补上了认它的那一支。
    ///
    /// 与 [`Self::access`] 分工分明：那本是**用户自己**登录换来的，认出来就
    /// 放行；这本带**语义作用域**，认出来之后还要问一句「这条路由它够不够
    /// 得着」—— 因为持有者是不可信代码。
    delegations: Arc<crate::delegated_token::DelegatedTokens>,
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
    /// 装配这个进程的全部依赖。
    ///
    /// # 为什么忍着这一长串参数，而不是收进一个配置结构体
    ///
    /// 收进结构体的直接后果是**漏配一项不再编译不过**：`Default` 一来，
    /// 「忘了给对象存储」与「这台机器故意不接对象存储」就长得一模一样，
    /// 而前者的症状是附件功能悄悄消失。位置参数难看，但它逼着每个构造点
    /// 对每一项表态 —— 测试里那几个 `None` 各自带着一行「为什么这组用例
    /// 不需要它」，正是这个约束换来的。
    ///
    /// 真要收，该收的是「这几个 `Option` 表达的其实是同一件事：这个部署开了
    /// 哪几项能力」，那是另一次改动。在那之前不加 `Default`。
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "见上：改成配置结构体会让「漏配一项」从编译错误退化成运行时静默缺失"
    )]
    pub fn new(
        runner: Arc<dyn SandboxRunner>,
        http: reqwest::Client,
        store: Option<cortex_store::Store>,
        accounts: Option<Accounts>,
        llm: Option<cortex_llm::LlmClient>,
        auth: AuthMode,
        // 新参数一律**追加在末尾**：这几个都是 `Option<…>`，中间插一个的话
        // 每个构造点都还编得过，只是把值传给了错的那一个字段
        blobs: Option<crate::blobs::MediaStore>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                runner,
                http,
                store,
                accounts,
                llm,
                blobs,
                auth,
                tickets: Arc::new(TicketBook::default()),
                access: Arc::new(crate::accounts::AccessBook::default()),
                auth_throttle: Arc::new(crate::rate_limit::AuthThrottle::default()),
                sync_buses: Arc::new(crate::sync_bus::SyncBuses::default()),
                delegations: Arc::new(crate::delegated_token::DelegatedTokens::default()),
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

    /// 同上，但给出一个能塞进 [`crate::request_tenant::Tenant`] 的所有权句柄。
    ///
    /// `None` = 这个部署没接数据库。**不要在这里回落到一个空 Store** ——
    /// 那会让「没配库」与「库是空的」在调用点长得一样。
    #[must_use]
    pub fn public_store(&self) -> Option<Arc<cortex_store::Store>> {
        // `Store` 内部是 `Arc<PgPool>` 的封装，clone 廉价；再包一层 Arc
        // 只是为了让 `Tenant` 能持有它而不必借用 self
        self.inner.store.clone().map(Arc::new)
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

    /// 服务端那把 key 组装出来的 LLM 客户端。
    ///
    /// 自带 key 那条路也要它 —— 用户填的是 key，不是模型，模型配置沿用
    /// 这一份（见 [`crate::llm`]）。
    ///
    /// # Errors
    /// 这个部署没配模型。回 501 而不是 panic：客户端把 501 当成「这条路
    /// 不会开」，据此降级并**不重试**，而那正是实情。
    pub fn llm(&self) -> Result<&cortex_llm::LlmClient, crate::error::ApiError> {
        self.inner.llm.as_ref().ok_or_else(|| {
            crate::error::ApiError::unsupported(
                "这个部署没有配 LLM 供应商（CORTEX_LLM_PROVIDER / 对应的 API key），\
                 借模型这条路不可用。本地 agent 可以配成直连供应商。",
            )
        })
    }

    /// 附件的字节存放处。
    ///
    /// # Errors
    /// 这个部署没接对象存储。回 501 而不是 500：客户端把 501 当成「这条路
    /// 不会开」，据此把附件入口整个降级掉并**不重试**，而那正是实情 ——
    /// 一台没有对象存储的机器，重试一万次也传不上去。
    pub fn blobs(&self) -> Result<&crate::blobs::MediaStore, crate::error::ApiError> {
        self.inner.blobs.as_ref().ok_or_else(|| {
            crate::error::ApiError::unsupported(
                "这个部署没有接对象存储（S3_ENDPOINT / S3_BUCKET / S3_REGION / \
                 RUSTFS_ACCESS_KEY / RUSTFS_SECRET_KEY 缺项，也没有 CORTEX_BLOB_DIR），\
                 附件功能不可用",
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

    /// 认证端点的限流表。检查与记录都走它，见 [`crate::rate_limit`]。
    #[must_use]
    pub fn auth_throttle(&self) -> &crate::rate_limit::AuthThrottle {
        &self.inner.auth_throttle
    }

    /// 按租户取实时推送总线。
    #[must_use]
    pub fn sync_buses(&self) -> &crate::sync_bus::SyncBuses {
        &self.inner.sync_buses
    }

    /// 委托令牌簿。签发走 [`crate::routes`]，校验走 [`crate::auth::require`]。
    #[must_use]
    pub fn delegations(&self) -> &crate::delegated_token::DelegatedTokens {
        &self.inner.delegations
    }

    #[must_use]
    pub fn runner(&self) -> &Arc<dyn SandboxRunner> {
        &self.inner.runner
    }

    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
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
