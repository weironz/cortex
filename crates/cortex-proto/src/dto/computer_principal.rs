//! Computer Use 的服务端权威 principal。
//!
//! 这些类型只描述**已经验证过**的结果。客户端提交的 `user_id`、
//! `deployment_id` 或目录名都不属于请求，服务端也不会把它们当作身份输入。

use serde::{Deserialize, Serialize};

/// local agent 请求为某个会话验证 Computer Use 主体。
///
/// `session_id = null` 表示只验证当前 access token 的部署与账号身份。这个形状
/// 只用于桌面端刚创建、远端还没有落库的会话草稿；它不会授权任何远端 session。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUsePrincipalRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub allow_new_session: bool,
}

/// 这份 principal 是由哪一类凭据证明的。
///
/// 当前只允许用户登录换来的 access token。保留枚举而不是布尔值，是为了
/// 将来若增加设备绑定凭据时，老客户端不能把未知类别静默当成 access。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseCredentialKind {
    AccessToken,
}

/// 这次 principal 验证绑定了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseSessionBinding {
    /// 服务端在严格凭据所属租户里确认了这个已有 session。
    Existing,
    /// 服务端只验证了部署与账号，本地 session 尚未落库。
    Identity,
}

/// 服务端验证出的严格 Computer Use 主体。
///
/// `deployment_url` 只用于连接与展示；授权身份始终是 `deployment_id`。
/// `expires_in_secs` 是 access token 在响应组装时剩余的有效秒数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUsePrincipal {
    pub deployment_id: String,
    pub deployment_url: String,
    pub user_id: String,
    pub session_id: Option<String>,
    pub credential_kind: ComputerUseCredentialKind,
    pub expires_in_secs: u64,
    pub session_binding: ComputerUseSessionBinding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_kind_has_a_stable_wire_value() {
        let json = serde_json::to_value(ComputerUseCredentialKind::AccessToken)
            .expect("凭据类别应当可序列化");
        assert_eq!(
            json,
            serde_json::json!("access_token"),
            "线上值必须固定为 access_token；改名会让老客户端把未知类别当成另一种凭据"
        );
    }

    #[test]
    fn request_rejects_client_supplied_identity_fields() {
        let error = serde_json::from_str::<ComputerUsePrincipalRequest>(
            r#"{"session_id":"s1","user_id":"forged"}"#,
        )
        .expect_err("严格入口不能让客户端往请求里塞 user_id");
        assert!(
            error.to_string().contains("unknown field"),
            "拒绝原因应明确指出请求包含未知字段，实际：{error}"
        );

        let principal = ComputerUsePrincipal {
            deployment_id: "a".repeat(64),
            deployment_url: "https://cortex.example/api".into(),
            user_id: "01USER".into(),
            session_id: Some("s1".into()),
            credential_kind: ComputerUseCredentialKind::AccessToken,
            expires_in_secs: 60,
            session_binding: ComputerUseSessionBinding::Existing,
        };
        let json = serde_json::to_string(&principal).expect("principal 应当可序列化");
        let decoded: ComputerUsePrincipal =
            serde_json::from_str(&json).expect("principal 应当可反序列化");
        assert_eq!(decoded, principal, "principal 的线上合同必须能无损往返");
    }

    #[test]
    fn identity_only_request_has_no_session() {
        let req: ComputerUsePrincipalRequest = serde_json::from_value(serde_json::json!({
            "session_id": null,
            "allow_new_session": false
        }))
        .expect("identity-only 请求形状应当可解析");

        assert_eq!(req.session_id, None);
        assert!(!req.allow_new_session);
    }

    #[test]
    fn identity_response_cannot_be_confused_with_an_existing_session() {
        let principal = ComputerUsePrincipal {
            deployment_id: "a".repeat(64),
            deployment_url: "https://cortex.example/api".into(),
            user_id: "01USER".into(),
            session_id: None,
            credential_kind: ComputerUseCredentialKind::AccessToken,
            expires_in_secs: 60,
            session_binding: ComputerUseSessionBinding::Identity,
        };

        let json = serde_json::to_value(principal).expect("principal 应当可序列化");
        assert_eq!(json["session_id"], serde_json::Value::Null);
        assert_eq!(json["session_binding"], "identity");
    }
}
