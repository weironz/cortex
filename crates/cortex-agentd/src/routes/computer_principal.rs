//! Computer Use 的严格服务端主体。
//!
//! 前置 [`crate::auth::require`] 仍要服务浏览器兼容路径，接受 PSK、ticket 与
//! delegated token。那些凭据足以打开某些既有端点，却不能证明「当前用户
//! 在这个部署上拥有这个会话」。local agent 在把 trace 或动作绑定到账号前，
//! 必须调用这里，并只接受本模块返回的类型化 principal。
//!
//! 本模块刻意不调用 `current_user`：那个函数为兼容旧部署，会在解析失败后
//! 回落到 owner。严格路径的每一次失败都必须是 401、404 或明确的服务错误。

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cortex_proto::dto::{
    ComputerUseCredentialKind, ComputerUsePrincipal, ComputerUsePrincipalRequest,
    ComputerUseSessionBinding,
};
use url::Url;

use crate::accounts::{AccessBook, AccessGrant};
use crate::auth::AuthMode;
use crate::error::ApiError;
use crate::state::AgentState;

const MAX_SESSION_ID_BYTES: usize = 256;
const AUTH_FAILED: &str = "Computer Use 主体验证失败：需要有效的用户 access token";

/// `POST /auth/computer-use-principal`
///
/// 请求可以绑定一个已有 `session_id`，也可以用 `session_id: null` 只验证
/// 当前 access token 的部署与账号；用户、租户和部署身份全部由服务端解析。
/// 响应禁止缓存，避免把某个账号的 principal 复用给后续 UI 会话。
pub async fn computer_use_principal(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<ComputerUsePrincipalRequest>,
) -> Result<Response, ApiError> {
    if req.allow_new_session {
        return Err(ApiError::bad_request(
            "allow_new_session 当前必须为 false；新会话要先做 identity-only 验证",
        ));
    }
    if let Some(session_id) = &req.session_id
        && (session_id.is_empty()
            || session_id.len() > MAX_SESSION_ID_BYTES
            || session_id.trim() != session_id)
    {
        return Err(ApiError::bad_request("session_id 形状非法"));
    }

    // 严格凭据判断必须在任何数据库或租户操作之前。
    let grant = resolve_access_grant(st.auth_mode(), st.access_book(), &headers)?;
    let accounts = st.accounts()?;
    // identity-only 也必须在数据库里证明该账号此刻仍存在且未停用。access
    // 簿子可能还留着刚被删用户或刚停用账号的短命 token，不能只看签名与到期。
    let tenant = st
        .tenant_for_active_user(grant.user_id())
        .await
        .map_err(|error| {
            if error.status() == StatusCode::UNAUTHORIZED {
                ApiError::unauthorized(AUTH_FAILED)
            } else {
                error
            }
        })?;

    let binding = if let Some(session_id) = req.session_id.as_deref() {
        // 这条链不会把未知用户回落到 public。查询成功代表用户此刻存在且未停用。
        let store = tenant
            .store()
            .map_err(|e| ApiError::internal(format!("严格 principal 无法连接租户库：{e}")))?;

        // `session_state` 覆盖只有生命周期事件的会话；`session_digest` 覆盖只有
        // episode 的旧数据。两个都不存在时，不区分「不存在」与「在别人的租户」。
        let belongs = store
            .session_state(session_id)
            .await
            .map_err(|e| ApiError::internal(format!("检查会话归属失败：{e}")))?
            .is_some();
        let belongs = if belongs {
            true
        } else {
            store
                .session_digest(session_id)
                .await
                .map_err(|e| ApiError::internal(format!("检查会话归属失败：{e}")))?
                .is_some()
        };
        if !belongs {
            return Err(ApiError::not_found("目标会话不存在，或不属于当前认证主体"));
        }
        ComputerUseSessionBinding::Existing
    } else {
        ComputerUseSessionBinding::Identity
    };

    // 数据库检查可能消耗掉 access 的最后几百毫秒。返回证明前再验一次；
    // 这里不做时钟取整，过期就是过期。
    if !grant.is_valid() || grant.expires_in_secs() == 0 {
        return Err(ApiError::unauthorized(AUTH_FAILED));
    }

    let principal = ComputerUsePrincipal {
        deployment_id: accounts.deployment_id().to_owned(),
        deployment_url: canonical_deployment_url(&headers),
        user_id: grant.user_id().to_owned(),
        session_id: req.session_id,
        credential_kind: ComputerUseCredentialKind::AccessToken,
        expires_in_secs: grant.expires_in_secs(),
        session_binding: binding,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(principal)).into_response())
}

fn resolve_access_grant(
    mode: &AuthMode,
    access: &AccessBook,
    headers: &HeaderMap,
) -> Result<AccessGrant, ApiError> {
    if matches!(mode, AuthMode::Disabled) {
        return Err(ApiError::unauthorized(AUTH_FAILED));
    }
    let token = strict_bearer(headers).ok_or_else(|| ApiError::unauthorized(AUTH_FAILED))?;
    let grant = access
        .strict_resolve(token)
        .ok_or_else(|| ApiError::unauthorized(AUTH_FAILED))?;
    if grant.expires_in_secs() == 0 {
        return Err(ApiError::unauthorized(AUTH_FAILED));
    }
    Ok(grant)
}

/// 只接受恰好一个 `Authorization: Bearer …` 首部。
///
/// 多写一个首部的请求没有唯一答案，按认证失败处理。查询串里的 ticket
/// 完全不读：中间件可以为浏览器兼容放行它，这里不能。
fn strict_bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty() && !token.contains(char::is_whitespace)).then_some(token)
}

/// 部署地址只帮助客户端找到服务，绝不是授权身份。
///
/// 优先使用服务端配置；没有配置时才从反代首部推导，与画廊公开链接采用同一
/// 拓扑。两种来源都只接受无凭据、无 query、无 fragment 的 HTTP(S) 地址。
fn canonical_deployment_url(headers: &HeaderMap) -> String {
    let configured = std::env::var("CORTEX_PUBLIC_URL").ok();
    canonical_deployment_url_with(headers, configured.as_deref())
}

fn canonical_deployment_url_with(headers: &HeaderMap, configured: Option<&str>) -> String {
    if let Some(raw) = configured
        && !raw.trim().is_empty()
    {
        if let Some(url) = normalize_url(raw) {
            return url;
        }
        tracing::warn!("CORTEX_PUBLIC_URL 不是合法的 HTTP(S) 部署地址，principal 将按请求首部推导");
    }

    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let proto = get("x-forwarded-proto")
        .filter(|value| value.eq_ignore_ascii_case("http") || value.eq_ignore_ascii_case("https"))
        .unwrap_or("http")
        .to_ascii_lowercase();
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or("127.0.0.1");
    let prefix = get("x-forwarded-prefix").unwrap_or("");
    let derived = format!("{proto}://{host}{prefix}");
    normalize_url(&derived).unwrap_or_else(|| "http://127.0.0.1".to_owned())
}

fn normalize_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Some(url.as_str().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn token_mode() -> AuthMode {
        let (_, digest_hex) = crate::auth::generate();
        let digest = hex::decode(digest_hex)
            .expect("生成摘要应是十六进制")
            .try_into()
            .expect("SHA-256 是 32 字节");
        AuthMode::Token { digest }
    }

    #[test]
    fn strict_entry_accepts_only_access_book_tokens() {
        let mode = token_mode();
        let book = AccessBook::default();
        let access = book.issue_for_test("01USER", Duration::from_secs(60));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {access}").parse().unwrap(),
        );
        let grant = resolve_access_grant(&mode, &book, &headers).expect("access token 应通过");
        assert_eq!(grant.user_id(), "01USER");

        headers.insert(
            header::AUTHORIZATION,
            "Bearer a-preshared-token".parse().unwrap(),
        );
        assert_eq!(
            resolve_access_grant(&mode, &book, &headers)
                .expect_err("PSK 不在 access 簿子里")
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn disabled_auth_and_ambiguous_headers_fail_closed() {
        let book = AccessBook::default();
        let access = book.issue_for_test("01USER", Duration::from_secs(60));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {access}").parse().unwrap(),
        );
        assert_eq!(
            resolve_access_grant(&AuthMode::Disabled, &book, &headers)
                .expect_err("认证关闭时严格入口也必须拒绝")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        headers.append(
            header::AUTHORIZATION,
            format!("Bearer {access}").parse().unwrap(),
        );
        assert_eq!(
            resolve_access_grant(&token_mode(), &book, &headers)
                .expect_err("重复 Authorization 首部没有唯一主体")
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn ticket_query_is_not_an_access_token() {
        let mut headers = HeaderMap::new();
        assert!(
            strict_bearer(&headers).is_none(),
            "只有 ?ticket= 的请求不能从本入口取得 principal"
        );
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert!(strict_bearer(&headers).is_none(), "非 Bearer 方案必须被拒");
    }

    #[test]
    fn deployment_url_is_normalized_and_never_contains_credentials() {
        assert_eq!(
            normalize_url("https://Cortex.Example/api/"),
            Some("https://cortex.example/api".into())
        );
        for bad in [
            "https://user:pass@example.com",
            "file:///tmp/x",
            "https://example.com/?token=secret",
            "https://example.com/#fragment",
        ] {
            assert!(
                normalize_url(bad).is_none(),
                "非法或带敏感成分的地址不能进入 principal：{bad}"
            );
        }

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "cortex.example".parse().unwrap());
        headers.insert("x-forwarded-prefix", "/api".parse().unwrap());
        assert_eq!(
            canonical_deployment_url_with(&headers, None),
            "https://cortex.example/api",
            "生产反代地址应推导为规范 URL"
        );
        assert_eq!(
            canonical_deployment_url_with(&headers, Some("https://configured.example/root/")),
            "https://configured.example/root",
            "服务端配置地址必须优先，并去掉末尾斜杠"
        );
    }
}
