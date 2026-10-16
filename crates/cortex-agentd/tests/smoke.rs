//! HTTP → 真 SQL 冒烟。
//!
//! # 覆盖什么、不覆盖什么
//!
//! 覆盖**「整条 HTTP 合同上有风险」的 happy path**：公开侧（health /
//! register / login / refresh）、三道防护的中间件、以及改动最密、最有
//! 前科的几个列表/写入端点（`/sessions`、`/library`、`/images`）。
//! 每条用例起码答一件事：这次部署的 schema、迁移、handler、bind 是
//! 同一套 —— gallery `$6` 那类 bug 要的正是这一句。
//!
//! 不覆盖：LLM 流（不配模型）、沙箱容器（PanicRunner 顶着）、WS（要真
//! 端口，内部单测已守）、附件字节（blobs 是单独一层，有自己的测试）。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{TestApp, app, unique_username};

/// 完整的一趟：匿名被拒 → 注册 → 登录 → 拿 token 打受保护的列表 →
/// refresh 换新 → 旧 refresh 重放被拒。
///
/// 一趟写在一个用例里而不是按端点拆：这些端点共享同一个账号，拆了要么
/// 各建各的（慢一倍）、要么借账号（互相污染）。冒烟要的是「这条路通」，
/// 不是每个端点的独立回归。
#[tokio::test]
async fn register_login_then_hit_protected_lists() {
    let Some(app) = app().await else { return };

    // ── 匿名必须被拒（受保护侧的一道代表）──
    let (status, _) = app.get("/sessions", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "/sessions 不带凭据必须 401，放行就是大门敞开"
    );

    // ── health 是公开的，给容器探针 ──
    let (status, _) = app.get("/health", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/health 必须公开可用（探针带不了凭据）"
    );

    // ── 注册 ──
    let username = unique_username("smoke");
    let password = "smoke-测试口令-123456";
    let (status, body) = app
        .post(
            "/auth/register",
            None,
            json!({"username": username, "password": password}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "开放注册的部署上注册应当 200，实际：{body}"
    );
    let access = body["access_token"]
        .as_str()
        .expect("注册应当直接发 access_token")
        .to_owned();
    let refresh = body["refresh_token"]
        .as_str()
        .expect("注册应当发 refresh_token")
        .to_owned();

    // ── whoami ──
    let (status, me) = app.get("/auth/me", Some(&access)).await;
    assert_eq!(status, StatusCode::OK, "自己的 token 应当认得出自己：{me}");
    assert_eq!(
        me["username"].as_str(),
        Some(username.as_str()),
        "whoami 答的应当是刚注册的那个人：{me}"
    );

    // ── 新账号的列表端点：空，但必须 200 且形状对 ──
    //
    // 「空列表」恰恰是 SQL 那条路全走到的形态：表在、迁移到最新、bind
    // 数对得上 —— 全都在测。断言结构性而非逐字段，免得接口加字段就红。
    for path in ["/sessions", "/library", "/images"] {
        let (status, body) = app.get(path, Some(&access)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "新账号 GET {path} 应当 200 空列表，实际 {status}：{body}"
        );
        assert!(
            body.is_array() || body.is_object(),
            "GET {path} 的应答应当是 JSON 结构而不是错误页：{body}"
        );
    }

    // ── 会话真写一条再列出来 ──
    //
    // 会话没有「POST /sessions 建一个」这条路由：它是事件流（sync / chat）
    // 的唯一写通道产生的。客户端就是这么落的 —— 一条 Rename 事件加一条
    // 消息进来，HTTP 列表就得看得见。比「空列表 200」多验半条：append-only
    // + sync_log 把序走对了（WriteTxn 一个事务两个动作）。
    //
    // ⚠️ 光写 Rename **列表里是空**：`session_digests` 从 episodes 聚合，
    // 一条「建了还没说过话」的会话在列表眼里不存在（sessions.rs 的
    // session_detail 注释专门记了这条不对称，它是真的）。
    //
    // 写进**这个账号自己的租户 schema**，不是 public —— 同一进程里如果已经
    // 有别的用例注过册，这位就不落在 public 上。
    let session_id = cortex_core::Id::new().to_string();
    let tenant = app.tenant_store(&username).await;
    tenant
        .write_txn(async |tx| {
            tx.insert_session_event(&cortex_store::NewSessionEvent::rename(
                &session_id,
                "冒烟会话",
                cortex_store::Actor::User,
                "cortex-smoke",
            ))
            .await
        })
        .await
        .expect("写会话事件不应失败");
    tenant
        .write_txn(async |tx| {
            tx.insert_episode(&cortex_store::NewEpisode {
                id: cortex_core::Id::new(),
                session_id: session_id.clone(),
                role: cortex_store::Role::User,
                content: serde_json::json!({"text": "冒烟一句"}),
                text: Some("冒烟一句".to_owned()),
                domain: None,
                device_id: "cortex-smoke".to_owned(),
                occurred_at: chrono::Utc::now(),
                models: Vec::new(),
            })
            .await
        })
        .await
        .expect("写消息不应失败");

    let (status, listed) = app.get("/sessions", Some(&access)).await;
    assert_eq!(status, StatusCode::OK);
    let serialized = serde_json::to_string(&listed).expect("序列化");
    assert!(
        serialized.contains("冒烟会话") && serialized.contains(&session_id),
        "写进去的会话应当列得出来：{serialized}"
    );

    // ── refresh：换新；宽限内的重放是幂等，宽限外的重放作废全家 ──
    //
    // 断言的是 2026-08-25 根因 C 修完之后的那套语义，不是它之前的：
    //
    //   1. 同一枚在 **30s 宽限内**再来一次 → 还**同一对**后继（幂等）。
    //      双进程（GUI + 本地 agent）共持一对凭据，立刻拒就会把对方踢出去。
    //   2. 过了宽限再来 → 判定重放 → **整条 family 作废**，连最新那把
    //      也一起失效。合法客户端手上只有最新那把，所以这是别人的信号。
    let (status, rotated) = app
        .post("/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(status, StatusCode::OK, "refresh 应当换新：{rotated}");
    let new_access = rotated["access_token"]
        .as_str()
        .expect("refresh 应答里该有新的 access_token")
        .to_owned();
    let new_refresh = rotated["refresh_token"]
        .as_str()
        .expect("refresh 应答里该有新的 refresh_token")
        .to_owned();

    // 宽限内：同一枚旧 token 再来一次 → 200 且**还是同一对**
    let (status, replayed_in_grace) = app
        .post("/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "宽限内的重放应当幂等还同一对，实际 {status}：{replayed_in_grace}"
    );
    assert_eq!(
        replayed_in_grace["refresh_token"].as_str(),
        Some(new_refresh.as_str()),
        "宽限内重放还的应当是同一对后继，实际是另一对（那说明幂等缓存漏了）：\
         {replayed_in_grace}"
    );

    // 把旧 token 的 rotated_at 拨到宽限之前，再来一次 → 真重放，作废全家
    sqlx::query(
        "UPDATE cortex_auth.auth_tokens SET rotated_at = now() - interval '35 seconds'
          WHERE user_id IN (SELECT id FROM cortex_auth.users WHERE username = $1)
            AND rotated_at IS NOT NULL",
    )
    .bind(&username)
    .execute(&app.accounts.pool)
    .await
    .expect("拨回 rotated_at 不应失败");

    let (status, replayed_outside) = app
        .post("/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "宽限外的重放必须拒（且作废全家），实际 {status}：{replayed_outside}"
    );

    // 连坐生效：刚换出来的**最新那把**也不该再能用
    let (status, after_revoke) = app
        .post("/auth/refresh", None, json!({"refresh_token": new_refresh}))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "family 作废之后，最新那把 refresh 也应当失效，实际 {status}：{after_revoke}"
    );

    // 但刚换出来的 access token 不该受牵连 —— 它的责任期本来就这么长
    let (status, me_after) = app.get("/auth/me", Some(&new_access)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "family 作废断的是「续期」，不是「还在效期内的 access」：{me_after}"
    );
}

/// 防护中间件在**真装配**下的扫面：匿名打一批受保护的路由，全得 401。
///
/// 与内部单测 `every_protected_route_rejects_anonymous_requests` 的分工：
/// 那条盯清单本身（加路由忘加防护），这条盯**接了真库的真 router**——
/// 中间件顺序、state 装配一旦有缝，这里第一时间红。
#[tokio::test]
async fn protected_routes_reject_anonymous() {
    let Some(app): Option<TestApp> = app().await else {
        return;
    };

    for path in ["/sessions", "/library", "/images", "/auth/me"] {
        let (status, _) = app.get(path, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET {path} 不带凭据必须 401，实际 {status}"
        );
    }
}
