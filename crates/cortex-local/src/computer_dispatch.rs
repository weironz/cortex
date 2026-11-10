//! 电脑工具的调用与真实审批关联。ID 只作关联，不替代账号、策略或持久派发闸。
//!
//! 模型返回的 tool_call id 会被供应商复用；操作 ID 必须来自 agent，在询问
//! 之前生成。同一次调用不能消费另一次调用的审批，也不能用同一个上下文重放。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use cortex_agent::ComputerInvocation;
use cortex_core::Id;
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_OPERATIONS: usize = 64;

tokio::task_local! {
    /// 由宿主建立，只在本次调用的 future 中有效，模型参数不能写入。
    pub(crate) static CURRENT_DISPATCH: DispatchBinding;
}

/// 原始审批 token 和输入文本都不进入派发关联。
pub(crate) struct DispatchBinding {
    pub(crate) operation_id: [u8; 16],
    pub(crate) approval_id: [u8; 16],
    pub(crate) arguments_hash: [u8; 32],
    pub(crate) user_confirmed: bool,
}

impl std::fmt::Debug for DispatchBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchBinding")
            .field("operation_id", &self.operation_id)
            .field("approval_id", &self.approval_id)
            .field("arguments_hash", &self.arguments_hash)
            .field("user_confirmed", &self.user_confirmed)
            .finish()
    }
}

struct ApprovedCall {
    tool: String,
    arguments_hash: [u8; 32],
    approval_id: [u8; 16],
}

#[derive(Default)]
struct State {
    pending: HashMap<Id, ApprovedCall>,
    consumed: HashSet<Id>,
}

#[derive(Default)]
pub(crate) struct DispatchCorrelation(Mutex<State>);

impl DispatchCorrelation {
    pub(crate) fn record_approved(
        &self,
        operation_id: Id,
        tool: &str,
        args: &Value,
        confirmation_token: &str,
    ) -> Result<(), String> {
        if confirmation_token.len() < 32 {
            return Err("审批关联缺少宿主生成的回执".to_owned());
        }
        let approval = ApprovedCall {
            tool: tool.to_owned(),
            arguments_hash: arguments_hash(args)?,
            approval_id: short_hash(
                b"cortex-computer-confirmation-v1\0",
                confirmation_token.as_bytes(),
            ),
        };
        let mut state = self.0.lock().map_err(|_| "审批关联锁已失效")?;
        if state.consumed.contains(&operation_id) || state.pending.contains_key(&operation_id) {
            return Err("操作已登记或已消费；不能替换它的审批".to_owned());
        }
        if state.pending.len() + state.consumed.len() >= MAX_OPERATIONS {
            return Err("本轮审批关联已达上限".to_owned());
        }
        state.pending.insert(operation_id, approval);
        Ok(())
    }

    pub(crate) fn bind(
        &self,
        invocation: &ComputerInvocation<'_>,
        tool: &str,
        args: &Value,
    ) -> Result<DispatchBinding, String> {
        self.bind_parts(
            *invocation.operation_id(),
            invocation.approved_by_user(),
            tool,
            args,
        )
    }

    fn bind_parts(
        &self,
        operation_id: Id,
        user_confirmed: bool,
        tool: &str,
        args: &Value,
    ) -> Result<DispatchBinding, String> {
        let arguments_hash = arguments_hash(args)?;
        let mut state = self.0.lock().map_err(|_| "审批关联锁已失效")?;
        if state.consumed.contains(&operation_id) {
            return Err("同一操作上下文已经消费；不能再次派发".to_owned());
        }
        let approval = state.pending.remove(&operation_id);
        if state.pending.len() + state.consumed.len() >= MAX_OPERATIONS {
            return Err("本轮操作关联已达上限".to_owned());
        }
        state.consumed.insert(operation_id);
        let approval_id = match (user_confirmed, approval) {
            (true, Some(approved))
                if approved.tool == tool && approved.arguments_hash == arguments_hash =>
            {
                approved.approval_id
            }
            (false, None) => short_hash(
                b"cortex-computer-policy-correlation-v1\0",
                operation_id.to_string().as_bytes(),
            ),
            _ => return Err("调用与实际审批回执不一致；没有派发电脑动作".to_owned()),
        };
        Ok(DispatchBinding {
            operation_id: short_hash(
                b"cortex-computer-operation-v1\0",
                operation_id.to_string().as_bytes(),
            ),
            approval_id,
            arguments_hash,
            user_confirmed,
        })
    }
}

fn short_hash(domain: &[u8], bytes: &[u8]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let hash = hasher.finalize();
    let mut result = [0; 16];
    result.copy_from_slice(&hash[..16]);
    result
}

fn arguments_hash(value: &Value) -> Result<[u8; 32], String> {
    // 不依赖 serde_json 是否启用了 preserve_order，键顺序不改变一次审批的含义。
    fn canonical(value: &Value, depth: usize) -> Result<Value, String> {
        if depth > 64 {
            return Err("电脑工具参数嵌套过深".to_owned());
        }
        match value {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort_unstable();
                let mut result = serde_json::Map::new();
                for key in keys {
                    result.insert(key.clone(), canonical(&map[key], depth + 1)?);
                }
                Ok(Value::Object(result))
            }
            Value::Array(values) => values
                .iter()
                .map(|value| canonical(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            _ => Ok(value.clone()),
        }
    }
    let encoded = serde_json::to_vec(&canonical(value, 0)?).map_err(|_| "电脑工具参数无法编码")?;
    let mut hash = Sha256::new();
    hash.update(b"cortex-computer-arguments-v1\0");
    hash.update(encoded);
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "test-only-confirmation-token-32-bytes-long";

    #[test]
    fn missing_actual_confirmation_cannot_be_replaced_by_a_boolean() {
        let tracker = DispatchCorrelation::default();
        let id = Id::new();
        assert!(
            tracker
                .bind_parts(id, true, "computer_act", &json!({}))
                .is_err(),
            "只有布尔批准，没有真实回执时必须拒绝"
        );
    }

    #[test]
    fn changed_parameters_consume_and_reject_the_receipt() {
        let tracker = DispatchCorrelation::default();
        let id = Id::new();
        tracker
            .record_approved(id, "computer_act", &json!({"text":"approved"}), TOKEN)
            .unwrap();
        assert!(
            tracker
                .bind_parts(id, true, "computer_act", &json!({"text":"changed"}))
                .is_err(),
            "换参数不能借用审批"
        );
        assert!(
            tracker
                .bind_parts(id, true, "computer_act", &json!({"text":"approved"}))
                .is_err(),
            "失配后的回执不能再次使用"
        );
    }

    #[test]
    fn distinct_host_operations_do_not_alias_receipts() {
        let tracker = DispatchCorrelation::default();
        for _ in 0..2 {
            let id = Id::new();
            tracker
                .record_approved(id, "computer_act", &json!({}), TOKEN)
                .unwrap();
            assert!(
                tracker
                    .bind_parts(id, true, "computer_act", &json!({}))
                    .is_ok(),
                "独立操作不应串用或阻断新的宿主审批"
            );
            assert!(
                tracker
                    .bind_parts(id, true, "computer_act", &json!({}))
                    .is_err(),
                "同一个宿主操作不能重放"
            );
        }
    }

    #[test]
    fn different_tool_cannot_consume_the_receipt() {
        let tracker = DispatchCorrelation::default();
        let id = Id::new();
        tracker
            .record_approved(id, "computer_act", &json!({}), TOKEN)
            .unwrap();
        assert!(
            tracker
                .bind_parts(id, true, "computer_browser_act", &json!({}))
                .is_err(),
            "审批必须绑定原工具"
        );
    }

    #[test]
    fn a_new_operation_cannot_borrow_an_earlier_receipt() {
        let tracker = DispatchCorrelation::default();
        tracker
            .record_approved(Id::new(), "computer_act", &json!({}), TOKEN)
            .unwrap();
        assert!(
            tracker
                .bind_parts(Id::new(), true, "computer_act", &json!({}))
                .is_err(),
            "即使参数相同，新操作也不能消费旧审批"
        );
    }

    #[test]
    fn pending_and_consumed_operations_share_the_bound() {
        let tracker = DispatchCorrelation::default();
        let ids: Vec<_> = (0..MAX_OPERATIONS).map(|_| Id::new()).collect();
        for id in &ids {
            tracker
                .record_approved(*id, "computer_act", &json!({}), TOKEN)
                .unwrap();
        }
        assert!(
            tracker
                .bind_parts(Id::new(), false, "computer_observe", &json!({}))
                .is_err(),
            "新调用不能绕过待确认项占用的上限"
        );
        assert!(
            tracker
                .bind_parts(ids[0], true, "computer_act", &json!({}))
                .is_ok(),
            "消费已登记项不应再占用一个名额"
        );
    }

    #[test]
    fn canonical_objects_match_but_array_order_does_not() {
        let a: Value = serde_json::from_str(r#"{"z":1,"a":{"y":2,"x":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":3,"y":2},"z":1}"#).unwrap();
        assert_eq!(
            arguments_hash(&a).unwrap(),
            arguments_hash(&b).unwrap(),
            "对象键顺序不能改变审批摘要"
        );
        assert_ne!(
            arguments_hash(&json!([1, 2])).unwrap(),
            arguments_hash(&json!([2, 1])).unwrap(),
            "动作数组顺序必须保留"
        );
    }
}
