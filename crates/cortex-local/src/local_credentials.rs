//! 本机控制凭据与远端账号凭据分开，设备控制权不转发给服务端。

use cortex_core::{CortexError, Result};

pub(crate) struct AgentCredentials {
    pub(crate) inbound: Option<String>,
    pub(crate) outbound: Option<String>,
}

impl AgentCredentials {
    pub(crate) fn inbound_fingerprint(&self) -> Option<String> {
        self.inbound.as_deref().map(cortex_core::token_fingerprint)
    }
    pub(crate) fn new(outbound: Option<String>, inbound: Option<String>) -> Result<Self> {
        let outbound = outbound.filter(|value| !value.trim().is_empty());
        let inbound = match inbound {
            Some(value) if value.trim().is_empty() => {
                return Err(CortexError::Config("显式本机入站凭据不能为空".into()));
            }
            Some(value) => Some(value),
            // 旧 CLI 没有独立控制凭据，保留其现有行为；桌面新客户端必须显式传入。
            None => outbound.clone(),
        };
        Ok(Self { inbound, outbound })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_control_key_never_becomes_remote_access_token() {
        let credentials =
            AgentCredentials::new(Some(String::new()), Some("local-test-control".into())).unwrap();
        assert!(
            credentials.outbound.is_none(),
            "清空出站值不能回落到设备控制凭据"
        );
        assert_eq!(
            credentials.inbound.as_deref(),
            Some("local-test-control"),
            "本机仍应有独立控制凭据"
        );
    }

    #[test]
    fn separate_credentials_preserve_direction_and_legacy_cli_still_works() {
        let credentials = AgentCredentials::new(
            Some("remote-test-access".into()),
            Some("local-test-control".into()),
        )
        .unwrap();
        assert_eq!(credentials.outbound.as_deref(), Some("remote-test-access"));
        assert_eq!(credentials.inbound.as_deref(), Some("local-test-control"));
        let legacy = AgentCredentials::new(Some("legacy-test".into()), None).unwrap();
        assert_eq!(
            legacy.inbound, legacy.outbound,
            "旧CLI只给一个token时保持兼容"
        );
    }

    #[test]
    fn explicit_empty_local_credential_cannot_disable_inbound_auth() {
        assert!(
            AgentCredentials::new(Some("remote-test".into()), Some(" ".into())).is_err(),
            "显式空控制凭据不能静默关闭本机认证"
        );
    }

    #[test]
    fn discovery_fingerprint_matches_transport_key_not_remote_account_token() {
        let credentials = AgentCredentials::new(
            Some("remote-test-access".into()),
            Some("local-test-control".into()),
        )
        .unwrap();
        assert_eq!(
            credentials.inbound_fingerprint(),
            Some(cortex_core::token_fingerprint("local-test-control")),
            "发现文件必须描述客户端实际要使用的入站凭据"
        );
        assert_ne!(
            credentials.inbound_fingerprint(),
            Some(cortex_core::token_fingerprint("remote-test-access")),
            "旧CLI不能把双凭据桌面实例认作单token实例"
        );
    }
}
