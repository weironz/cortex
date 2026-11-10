//! Computer Use 严格服务端主体的真 HTTP → 真 SQL 回归。
//!
//! 这些用例只走 agentd 的认证与租户链；trace 的写入、读取和本机存储不在
//! 这里，也不由这个入口顺带证明。

mod common;

use std::sync::Arc;

use axum::http::{StatusCode, header};
use cortex_agentd::delegated_token::DelegatedScope;
use cortex_store::Store;
use serde_json::{Value, json};

use common::{TestApp, app, app_with_disabled_auth, unique_username};

struct RegisteredUser {
    username: String,
    user_id: String,
    access: String,
}

async fn register(app: &TestApp, tag: &str) -> RegisteredUser {
    let username = unique_username(tag);
    let password = "computer-principal-测试口令-123456";
    let (status, body) = app
        .post(
            "/auth/register",
            None,
            json!({"username": username, "password": password}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "注册测试账号应当成功：{body}");
    RegisteredUser {
        username,
        user_id: body["user_id"]
            .as_str()
            .expect("注册响应必须带 user_id")
            .to_owned(),
        access: body["access_token"]
            .as_str()
            .expect("注册响应必须带 access_token")
            .to_owned(),
    }
}

async fn write_session_state(store: &Arc<Store>, session_id: &str) {
    store
        .write_txn(async |tx| {
            tx.insert_session_event(&cortex_store::NewSessionEvent::rename(
                session_id,
                "principal 测试会话",
                cortex_store::Actor::User,
                "computer-principal-test",
            ))
            .await
        })
        .await
        .expect("写会话生命周期事件不应失败");
}

async fn write_episode(store: &Arc<Store>, session_id: &str) {
    store
        .write_txn(async |tx| {
            tx.insert_episode(&cortex_store::NewEpisode {
                id: cortex_core::Id::new(),
                session_id: session_id.to_owned(),
                role: cortex_store::Role::User,
                content: json!({"text": "只有 episode 的旧会话"}),
                text: Some("只有 episode 的旧会话".to_owned()),
                domain: None,
                device_id: "computer-principal-test".to_owned(),
                occurred_at: chrono::Utc::now(),
                models: Vec::new(),
            })
            .await
        })
        .await
        .expect("写 episode 不应失败");
}

fn assert_principal_shape(
    body: &Value,
    user: &RegisteredUser,
    session_id: &str,
    deployment_id: &str,
) {
    assert_eq!(body["deployment_id"].as_str(), Some(deployment_id));
    assert_eq!(body["user_id"].as_str(), Some(user.user_id.as_str()));
    assert_eq!(body["session_id"].as_str(), Some(session_id));
    assert_eq!(body["credential_kind"].as_str(), Some("access_token"));
    assert_eq!(
        body["session_binding"].as_str(),
        Some("existing"),
        "已有会话验证必须与 identity-only 明确区分"
    );
    assert!(
        body["expires_in_secs"]
            .as_u64()
            .is_some_and(|secs| secs > 0),
        "principal 必须带仍未到期的正数剩余寿命：{body}"
    );
    let url = body["deployment_url"]
        .as_str()
        .expect("principal 必须带部署地址");
    let parsed = url::Url::parse(url).expect("部署地址必须能解析");
    assert!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
        "部署地址必须是可连接的 HTTP(S) URL：{url}"
    );
    assert!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "部署地址不能携带凭据：{url}"
    );
    assert_eq!(
        deployment_id.len(),
        64,
        "deployment_id 必须是 32 字节随机值"
    );
    assert!(
        deployment_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "deployment_id 必须是小写十六进制：{deployment_id}"
    );
}

fn assert_identity_principal_shape(body: &Value, user: &RegisteredUser, deployment_id: &str) {
    assert_eq!(body["deployment_id"].as_str(), Some(deployment_id));
    assert_eq!(body["user_id"].as_str(), Some(user.user_id.as_str()));
    assert!(
        body["session_id"].is_null(),
        "identity-only 响应不能意外授权任何远端 session：{body}"
    );
    assert_eq!(body["credential_kind"].as_str(), Some("access_token"));
    assert_eq!(
        body["session_binding"].as_str(),
        Some("identity"),
        "新会话草稿只能绑定账号身份，不能伪造 existing/new session"
    );
    assert!(
        body["expires_in_secs"]
            .as_u64()
            .is_some_and(|secs| secs > 0),
        "identity-only principal 也必须带正数剩余寿命：{body}"
    );
    assert_eq!(
        deployment_id.len(),
        64,
        "deployment_id 必须是 32 字节随机值"
    );
}

/// access token 是唯一能取得 principal 的凭据；目标 session 必须能在
/// **该 token 对应的租户池**中证明存在，而且不存在与跨租户必须同形。
#[tokio::test]
async fn strict_principal_binds_access_user_session_and_deployment() {
    let Some(app) = app().await else { return };
    let alice = register(&app, "principal_a").await;
    let bob = register(&app, "principal_b").await;

    let alice_store = app.tenant_store(&alice.username).await;
    let bob_store = app.tenant_store(&bob.username).await;
    let lifecycle_session = cortex_core::Id::new().to_string();
    let legacy_episode_session = cortex_core::Id::new().to_string();
    let bob_session = cortex_core::Id::new().to_string();
    write_session_state(&alice_store, &lifecycle_session).await;
    write_episode(&alice_store, &legacy_episode_session).await;
    write_session_state(&bob_store, &bob_session).await;

    for session_id in [&lifecycle_session, &legacy_episode_session] {
        let (status, headers, body) = app
            .call_with_headers(
                "POST",
                "/auth/computer-use-principal",
                Some(&alice.access),
                Some(json!({"session_id": session_id})),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "本用户 session 应取得严格 principal：{body}"
        );
        assert_principal_shape(&body, &alice, session_id, app.accounts.deployment_id());
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "principal 绑定单个用户与短命凭据，响应不能被任何层缓存"
        );
    }

    let (identity_status, identity_headers, identity_body) = app
        .call_with_headers(
            "POST",
            "/auth/computer-use-principal",
            Some(&alice.access),
            Some(json!({"session_id": null, "allow_new_session": false})),
        )
        .await;
    assert_eq!(
        identity_status,
        StatusCode::OK,
        "本地新建但远端尚未落库的会话需要独立的 identity-only 验证：{identity_body}"
    );
    assert_identity_principal_shape(&identity_body, &alice, app.accounts.deployment_id());
    assert_eq!(
        identity_headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "identity-only principal 同样不能被缓存"
    );

    let (allow_new, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&alice.access),
            json!({"session_id": null, "allow_new_session": true}),
        )
        .await;
    assert_eq!(
        allow_new,
        StatusCode::BAD_REQUEST,
        "当前服务端不能由客户端声明一个新远端 session 已获授权"
    );

    let (cross_tenant, cross_headers, cross_body) = app
        .call_with_headers(
            "POST",
            "/auth/computer-use-principal",
            Some(&alice.access),
            Some(json!({"session_id": bob_session})),
        )
        .await;
    let missing_id = cortex_core::Id::new().to_string();
    let (missing, missing_headers, missing_body) = app
        .call_with_headers(
            "POST",
            "/auth/computer-use-principal",
            Some(&alice.access),
            Some(json!({"session_id": missing_id})),
        )
        .await;
    assert_eq!(
        cross_tenant,
        StatusCode::NOT_FOUND,
        "跨租户 session 必须拒绝"
    );
    assert_eq!(missing, StatusCode::NOT_FOUND, "不存在的 session 必须拒绝");
    assert_eq!(
        cross_body, missing_body,
        "跨租户与不存在必须使用同一个错误体，避免据此枚举别人的 session"
    );
    for headers in [&cross_headers, &missing_headers] {
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "不存在与跨主体的错误响应也不能被缓存"
        );
    }

    for field in ["user_id", "deployment_id", "tenant"] {
        let mut request = json!({"session_id": lifecycle_session});
        request[field] = json!("forged");
        let (unknown_field, _) = app
            .post("/auth/computer-use-principal", Some(&alice.access), request)
            .await;
        assert_eq!(
            unknown_field,
            StatusCode::UNPROCESSABLE_ENTITY,
            "客户端不能自己提交 {field}"
        );
    }

    let (unknown_token, unknown_headers, _) = app
        .call_with_headers(
            "POST",
            "/auth/computer-use-principal",
            Some("not-an-issued-access-token"),
            Some(json!({"session_id": lifecycle_session})),
        )
        .await;
    assert_eq!(unknown_token, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unknown_headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "认证失败响应也必须禁止缓存"
    );
    let (unknown_identity_token, _) = app
        .post(
            "/auth/computer-use-principal",
            Some("not-an-issued-access-token"),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(
        unknown_identity_token,
        StatusCode::UNAUTHORIZED,
        "identity-only 不能让未知 token 绕过 access token 验证"
    );

    let (psk_status, _) = app
        .post(
            "/auth/computer-use-principal",
            app.psk.as_deref(),
            json!({"session_id": lifecycle_session}),
        )
        .await;
    assert_eq!(
        psk_status,
        StatusCode::UNAUTHORIZED,
        "预共享 token 已通过前置中间件，但严格入口仍必须拒绝"
    );
    let (psk_identity, _) = app
        .post(
            "/auth/computer-use-principal",
            app.psk.as_deref(),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(psk_identity, StatusCode::UNAUTHORIZED);

    let delegated = app.state.delegations().issue(DelegatedScope {
        owner: alice.user_id.clone(),
        session_id: lifecycle_session.clone(),
        project: None,
    });
    let (delegated_status, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&delegated),
            json!({"session_id": lifecycle_session}),
        )
        .await;
    assert_eq!(
        delegated_status,
        StatusCode::FORBIDDEN,
        "委托凭据不能替沙箱取得用户级 principal"
    );
    let (delegated_identity, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&delegated),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(delegated_identity, StatusCode::FORBIDDEN);

    let ticket = app.state.ticket_book().issue(&alice.user_id);
    let ticket_path = format!("/auth/computer-use-principal?ticket={ticket}");
    let (ticket_status, _) = app
        .post(&ticket_path, None, json!({"session_id": lifecycle_session}))
        .await;
    assert_eq!(
        ticket_status,
        StatusCode::UNAUTHORIZED,
        "短命票据能让前置中间件放行，但不能证明用户 access token"
    );
    let (ticket_identity, _) = app
        .post(
            &ticket_path,
            None,
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(ticket_identity, StatusCode::UNAUTHORIZED);
}

/// disabled、未知主体与 auth-disabled 都 fail closed，不能回落 owner/public。
#[tokio::test]
async fn strict_principal_rejects_disabled_unknown_and_auth_disabled_subjects() {
    let Some(app) = app().await else { return };

    let disabled = register(&app, "principal_disabled").await;
    let disabled_store = app.tenant_store(&disabled.username).await;
    let disabled_session = cortex_core::Id::new().to_string();
    write_session_state(&disabled_store, &disabled_session).await;
    sqlx::query("UPDATE cortex_auth.users SET disabled_at = now() WHERE id = $1")
        .bind(&disabled.user_id)
        .execute(&app.accounts.pool)
        .await
        .expect("停用测试账号不应失败");
    let (disabled_status, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&disabled.access),
            json!({"session_id": disabled_session}),
        )
        .await;
    assert_eq!(
        disabled_status,
        StatusCode::UNAUTHORIZED,
        "access token 还活着但账号已停用时必须拒绝"
    );
    let (disabled_identity, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&disabled.access),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(
        disabled_identity,
        StatusCode::UNAUTHORIZED,
        "账号停用后 identity-only 也必须 fail closed"
    );

    let unknown = register(&app, "principal_unknown").await;
    let unknown_store = app.tenant_store(&unknown.username).await;
    let unknown_session = cortex_core::Id::new().to_string();
    write_session_state(&unknown_store, &unknown_session).await;
    sqlx::query("DELETE FROM cortex_auth.auth_tokens WHERE user_id = $1")
        .bind(&unknown.user_id)
        .execute(&app.accounts.pool)
        .await
        .expect("删除测试账号的 refresh 记录不应失败");
    sqlx::query("DELETE FROM cortex_auth.users WHERE id = $1")
        .bind(&unknown.user_id)
        .execute(&app.accounts.pool)
        .await
        .expect("删除测试账号不应失败");
    let (unknown_subject, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&unknown.access),
            json!({"session_id": unknown_session}),
        )
        .await;
    assert_eq!(
        unknown_subject,
        StatusCode::UNAUTHORIZED,
        "access 簿子里还认得的 token 若指向已不存在的用户，也不能回落 public"
    );
    let (unknown_identity_subject, _) = app
        .post(
            "/auth/computer-use-principal",
            Some(&unknown.access),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(unknown_identity_subject, StatusCode::UNAUTHORIZED);

    let Some(disabled_auth_app) = app_with_disabled_auth().await else {
        return;
    };
    let auth_disabled = register(&disabled_auth_app, "principal_auth_disabled").await;
    let disabled_auth_store = disabled_auth_app
        .tenant_store(&auth_disabled.username)
        .await;
    let auth_disabled_session = cortex_core::Id::new().to_string();
    write_session_state(&disabled_auth_store, &auth_disabled_session).await;
    let (auth_disabled_status, _) = disabled_auth_app
        .post(
            "/auth/computer-use-principal",
            Some(&auth_disabled.access),
            json!({"session_id": auth_disabled_session}),
        )
        .await;
    assert_eq!(
        auth_disabled_status,
        StatusCode::UNAUTHORIZED,
        "AuthMode::Disabled 只能跳过兼容中间件，不能生成 Computer Use 主体"
    );
    let (auth_disabled_identity, _) = disabled_auth_app
        .post(
            "/auth/computer-use-principal",
            Some(&auth_disabled.access),
            json!({"session_id": null, "allow_new_session": false}),
        )
        .await;
    assert_eq!(auth_disabled_identity, StatusCode::UNAUTHORIZED);
}
