//! HTTP → 真 SQL 冒烟测试的脚手架。
//!
//! # 为什么有这一层
//!
//! docs/quality-review-20260825.md 根因 A：agentd 40+ 条路由曾经全是
//! **内部单测** —— 构造一个不带库的 state，断言「匿名 401」。那类测试
//! 抓得到安全不变量，抓不到「route 声明了 6 个占位符、代码只 bind 了
//! 5 个」—— 那种 bug 只在「HTTP → handler → 真 SQL」整条路上现形
//! （0.1.x 的画廊 `$6` 就是）。这一层把「真库 + 真账号 + 真 router」
//! 装进一次 [`app`] 调用，用例本体只剩请求与断言。
//!
//! # 为什么 runner 是 `PanicRunner` 而不是 Docker 替身
//!
//! 冒烟不走 `/sandbox/*`：那半边的合同（spec 不可协商、不出网、spec_hash）
//! 由 `runner` 模块内部的规格测试守着，也不需要库。这里配一个**被调到就
//! panic** 的实现 —— 如果哪条用例碰了沙箱，panic 名正言顺地红，而不是
//! 悄悄打到宿主 docker daemon 上拉真实的容器。

#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cortex_agentd::runner::{DirEntry, SandboxHandle, SandboxRunner};
use cortex_agentd::state::{Accounts, AgentState};
use cortex_agentd::{auth::AuthMode, routes};
use cortex_core::Result;
use tower::ServiceExt as _;

/// 被调到就 panic 的 Runner。冒烟不碰沙箱；碰到就是装配漏了。
struct PanicRunner;

#[async_trait::async_trait]
impl SandboxRunner for PanicRunner {
    async fn ensure(&self, scope: &str, _token: &str, _spec: &str) -> Result<SandboxHandle> {
        panic!("冒烟不该碰沙箱，ensure 被调了：scope={scope}");
    }
    async fn stop(&self, scope: &str) -> Result<()> {
        panic!("冒烟不该碰沙箱，stop 被调了：scope={scope}");
    }
    async fn status(&self, scope: &str) -> Result<Option<SandboxHandle>> {
        panic!("冒烟不该碰沙箱，status 被调了：scope={scope}");
    }
    async fn export_workspace(&self, scope: &str) -> Result<bytes::Bytes> {
        panic!("冒烟不该碰沙箱，export_workspace 被调了：scope={scope}");
    }
    async fn import_workspace(&self, scope: &str, _tar: bytes::Bytes) -> Result<()> {
        panic!("冒烟不该碰沙箱，import_workspace 被调了：scope={scope}");
    }
    async fn list_dir(&self, scope: &str, path: &str) -> Result<Vec<DirEntry>> {
        panic!("冒烟不该碰沙箱，list_dir 被调了：scope={scope} path={path}");
    }
    async fn read_file(&self, scope: &str, path: &str) -> Result<bytes::Bytes> {
        panic!("冒烟不该碰沙箱，read_file 被调了：scope={scope} path={path}");
    }
    async fn write_file(&self, scope: &str, path: &str, _b: bytes::Bytes) -> Result<()> {
        panic!("冒烟不该碰沙箱，write_file 被调了：scope={scope} path={path}");
    }
    fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>> {
        None
    }
    async fn workspace_bytes(&self, _scope: &str) -> Result<Option<u64>> {
        Ok(None)
    }
    fn callback(&self) -> &str {
        "http://unreachable.invalid"
    }
    async fn callback_visible(&self) -> bool {
        false
    }
}

/// 一个起好的 router + 一把直连 `public` schema 的 store。
///
/// router 打 HTTP 那一半；store 走**唯一写通道**造数据（客户端产物都是
/// 这么来的 —— 会话没有一个 `POST /sessions` 创建路由，它是事件流出来的）。
pub struct TestApp {
    pub router: Router,
    pub store: cortex_store::Store,
    pub accounts: cortex_agentd::state::Accounts,
}

impl TestApp {
    /// 某个账号自己的租户 store（按它注册的 schema 连）。
    ///
    /// 不能假设它在 `public`：只有**全库第一个**账号落在 public，其他
    /// 都在 `u_…` derive 出来的 schema（accounts.rs 那段「老用户的家当
    /// 不能消失」的注释）。CI 的库虽然是新的，同一个进程里别的用例可能
    /// 先注过册 —— 往错了的 schema 里写，读端永远看不见，且**不报错**。
    pub async fn tenant_store(&self, username: &str) -> Arc<cortex_store::Store> {
        let schema: String =
            sqlx::query_scalar("SELECT schema_name FROM cortex_auth.users WHERE username = $1")
                .bind(username)
                .fetch_one(&self.accounts.pool)
                .await
                .expect("刚注册的账号应当查得到 schema");
        let name = cortex_store::SchemaName::new(&schema).expect("库里的 schema 名应当合法");
        self.accounts
            .tenants
            .get(&name)
            .await
            .expect("租户池应当连得上")
    }
}

fn database_url() -> Option<String> {
    let _ = dotenvy::dotenv();
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            // 与 cortex-store/tests 同一道：CI 里缺库是闸门断了，必须红；
            // 开发机没起数据库则允许跳过。`CI=true` 由 GitHub Actions 恒设。
            assert!(
                std::env::var("CI").is_err(),
                "CI 环境必须提供 DATABASE_URL（配 postgres service）；\
                 本地开发缺库跳过是允许的，CI 不行"
            );
            eprintln!("跳过：未设置 DATABASE_URL");
            None
        }
    }
}

/// 装配真库 + 真账号体系（库是 CI 的 service 库，migration 幂等）。
///
/// 顺序与 `main()` 同一道：**先 `Store::migrate()` 再上 `Accounts`** ——
/// 全局 schema 引用的 `ulid` / `sha256` 两个 DOMAIN 是租户 migration 建在
/// `public` 里的，顺序反了会炸在「type ulid does not exist」这种看着像
/// 数据库坏掉的错上。
pub async fn app() -> Option<TestApp> {
    let url = database_url()?;
    let store = cortex_store::Store::connect(&url)
        .await
        .expect("连得上测试库");
    store.migrate().await.expect("公共 migration 应能跑通");
    let accounts = Accounts::connect(&url).await.expect("全局 schema 应能跑通");

    // 注册开关是进程级配置：设一次，整份冒烟共享；**只设不清**，
    // 免得用例之间互相踩环境变量。
    set_open_registration_once();

    let st = AgentState::new(
        Arc::new(PanicRunner),
        reqwest::Client::new(),
        Some(store.clone()),
        Some(accounts.clone()),
        // 不配模型：/llm/* 不在冒烟范围；配了的话一不留神断言会变成
        // 一次打向供应商的几十秒超时
        None,
        // AuthMode::Disabled 的话中间件整层短路，连「匿名被拒」都验不到；
        // 给一把没人知道的 digest，门是真的在
        test_auth_mode(),
        None,
    );

    Some(TestApp {
        router: routes::router(st),
        store,
        accounts,
    })
}

fn set_open_registration_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 每个测试进程只在第一次装配时设一次，此后只读不写 ——
        // 等同于「进程带着这个环境变量启动」。
        unsafe { std::env::set_var("CORTEX_OPEN_REGISTRATION", "enabled") };
    });
}

fn test_auth_mode() -> AuthMode {
    let (_plain, digest_hex) = cortex_agentd::auth::generate();
    let raw = hex::decode(digest_hex).expect("摘要应当是合法十六进制");
    let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
    AuthMode::Token { digest }
}

impl TestApp {
    /// 打一发请求，拿 (status, json)。走完整 router（含认证中间件），
    /// 与内部单测的 oneshot 同一形状，只是这里的 state 接了真库。
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = match body {
            Some(v) => Body::from(serde_json::to_vec(&v).expect("序列化请求体不该失败")),
            None => Body::empty(),
        };
        let req = req
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .expect("构造请求不该失败");
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .expect("读响应体不该失败");
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    pub async fn get(&self, path: &str, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
        self.call("GET", path, bearer, None).await
    }

    pub async fn post(
        &self,
        path: &str,
        bearer: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        self.call("POST", path, bearer, Some(body)).await
    }
}

/// 测试进程内唯一的用户名（形状过 `users_username_shape` 那条 CHECK）。
pub fn unique_username(tag: &str) -> String {
    format!(
        "{}_{}",
        tag,
        cortex_core::Id::new().to_string().to_lowercase()
    )
}
