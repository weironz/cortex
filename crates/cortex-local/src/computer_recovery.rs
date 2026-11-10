//! 用户控制面专用的两次确认。模型工具、attach 和完全放行模式不签发恢复授权。
//! nonce 只关联用户预览；真正授权还要严格账号证明、当前凭据及原 custody 同时成立。

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::computer_input::RecoveryCandidate;
use crate::computer_principal::PrincipalNamespace;
use crate::remote::Remote;
use crate::runs::run_principal::CredentialSnapshot;
use crate::state::LocalState;

const CONSENT_TTL: Duration = Duration::from_secs(30);
const PREVIEW_TTL: Duration = Duration::from_secs(180);

#[derive(Clone)]
struct Owner {
    subject: PrincipalNamespace,
    credential: CredentialSnapshot,
    remote: Remote,
}

/// 仅本模块在严格身份验证及消费当前 nonce 后构造，不接受 JSON/digest 自证。
pub(crate) struct RecoveryConsent {
    owner: Owner,
    binding: [u8; 32],
    audit: [u8; 32],
    expires: Instant,
}
impl RecoveryConsent {
    pub(crate) fn audit_digest(&self) -> [u8; 32] {
        self.audit
    }

    // 只能在 guard 持有账号快照读锁期间用于后续短提交，避免重入账号锁与切号写者互等。
    pub(crate) fn check_binding(&self, candidate: &RecoveryCandidate) -> Result<(), String> {
        if Instant::now() >= self.expires || candidate.binding_digest() != self.binding {
            return Err("恢复确认已过期或不属于原操作；保持暂停".into());
        }
        candidate.validate()?;
        if candidate.operation().subject != self.owner.subject {
            return Err("恢复确认不属于原账号；保持暂停".into());
        }
        Ok(())
    }

    pub(crate) fn guard<T>(
        &self,
        candidate: &RecoveryCandidate,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.check_binding(candidate)?;
        self.owner
            .remote
            .with_exact_computer_credential(&self.owner.subject, &self.owner.credential, || {
                if Instant::now() >= self.expires {
                    return Err("恢复确认已过期；保持暂停".into());
                }
                action()
            })
            .map_err(|_| "账号或凭据已改变；保持暂停".to_owned())?
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Unavailable,
    NeedsStop,
    Working,
    NeedsReview,
    Complete,
}

#[derive(Serialize)]
struct View {
    phase: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u64>,
    message: String,
}
impl View {
    fn unavailable(message: &str) -> Self {
        Self {
            phase: Phase::Unavailable,
            recovery_id: None,
            operation_id: None,
            epoch: None,
            message: message.into(),
        }
    }
}

struct Entry {
    candidate: Option<RecoveryCandidate>,
    binding: [u8; 32],
    subject: PrincipalNamespace,
    session: String,
    principal_epoch: u64,
    credential_generation: u64,
    nonce: String,
    operation: String,
    epoch: u64,
    phase: Phase,
    expires: Instant,
    message: String,
}
impl Entry {
    fn view(&self) -> View {
        View {
            phase: self.phase,
            recovery_id: Some(self.nonce.clone()),
            operation_id: Some(self.operation.clone()),
            epoch: Some(self.epoch),
            message: self.message.clone(),
        }
    }
    fn belongs(&self, owner: &Owner, session: &str) -> bool {
        self.subject == owner.subject
            && self.session == session
            && self.principal_epoch == owner.credential.principal_epoch()
    }
}

#[derive(Default)]
struct Service {
    entry: Mutex<Option<Entry>>,
}
fn service() -> &'static Arc<Service> {
    static INSTANCE: OnceLock<Arc<Service>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(Service::default()))
}

#[derive(Deserialize)]
struct PreviewQuery {
    session_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    session_id: String,
    recovery_id: String,
    result: Option<String>,
}

pub(crate) fn router() -> Router<LocalState> {
    Router::new()
        .route("/local/computer-input/recovery", get(preview))
        .route("/local/computer-input/recovery/retire", post(retire))
        .route(
            "/local/computer-input/recovery/acknowledge",
            post(acknowledge),
        )
        .layer(axum::middleware::map_response(no_store))
}

async fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

async fn verified_owner(st: &LocalState, session: &str) -> Result<Owner, Box<Response>> {
    if st
        .inbound_token
        .as_ref()
        .is_none_or(|token| token.len() < 24)
    {
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                Json(View::unavailable("恢复入口需要已认证的本机客户端")),
            )
                .into_response(),
        ));
    }
    let principal = st
        .remote
        .verify_current_existing_session_principal_bound(session)
        .await
        .map_err(|_| {
            Box::new(
                (
                    StatusCode::FORBIDDEN,
                    Json(View::unavailable(
                        "无法验证当前账号与已有会话，电脑控制保持暂停",
                    )),
                )
                    .into_response(),
            )
        })?;
    let subject = principal
        .computer_namespace()
        .map_err(|_| {
            Box::new(
                (
                    StatusCode::FORBIDDEN,
                    Json(View::unavailable("恢复需要严格验证的账号")),
                )
                    .into_response(),
            )
        })?
        .clone();
    Ok(Owner {
        subject,
        credential: principal.credential().clone(),
        remote: st.remote.clone(),
    })
}

fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "无法生成恢复确认凭据")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn consent(owner: Owner, entry: &Entry, step: &[u8]) -> RecoveryConsent {
    let mut hash = Sha256::new();
    hash.update(b"cortex-input-recovery-consent-v1\0");
    hash.update(entry.binding);
    hash.update(entry.nonce.as_bytes());
    hash.update(step);
    hash.update(owner.credential.generation().to_le_bytes());
    RecoveryConsent {
        owner,
        binding: entry.binding,
        audit: hash.finalize().into(),
        expires: Instant::now() + CONSENT_TTL,
    }
}

async fn preview(State(st): State<LocalState>, Query(query): Query<PreviewQuery>) -> Response {
    let owner = match verified_owner(&st, &query.session_id).await {
        Ok(v) => v,
        Err(e) => return *e,
    };
    // 已确认的完成状态只来自本进程亲自收到的提交结果，不从持久 Active 快照推测。
    {
        let slot = service().entry.lock().await;
        if let Some(entry) = slot.as_ref()
            && entry.belongs(&owner, &query.session_id)
            && entry.phase == Phase::Working
        {
            return Json(entry.view()).into_response();
        }
    }
    let subject = owner.subject.clone();
    let candidate = match tokio::task::spawn_blocking(move || {
        crate::computer_ledger::recovery_candidate(&subject)
    })
    .await
    {
        Ok(Ok(Some(candidate))) => candidate,
        Ok(Ok(None)) => {
            let slot = service().entry.lock().await;
            if let Some(entry) = slot.as_ref()
                && entry.belongs(&owner, &query.session_id)
                && entry.phase == Phase::Complete
                && !crate::computer_ledger::has_recovery_custody()
            {
                return Json(entry.view()).into_response();
            }
            return Json(View::unavailable(
                "当前没有可由本进程安全恢复的操作；已有暂停不会自动解除",
            ))
            .into_response();
        }
        _ => {
            return Json(View::unavailable(
                "当前没有可由本进程安全恢复的操作；已有暂停不会自动解除",
            ))
            .into_response();
        }
    };
    let mut slot = service().entry.lock().await;
    if let Some(entry) = slot.as_mut()
        && entry.belongs(&owner, &query.session_id)
        && entry.binding == candidate.binding_digest()
    {
        if Instant::now() >= entry.expires
            || entry.credential_generation != owner.credential.generation()
        {
            entry.nonce = match nonce() {
                Ok(v) => v,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            entry.expires = Instant::now() + PREVIEW_TTL;
            entry.credential_generation = owner.credential.generation();
        }
        return Json(entry.view()).into_response();
    }
    if slot
        .as_ref()
        .is_some_and(|entry| entry.phase == Phase::Working)
    {
        return (
            StatusCode::CONFLICT,
            Json(View::unavailable("已有恢复正在处理")),
        )
            .into_response();
    }
    let nonce = match nonce() {
        Ok(v) => v,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let entry = Entry {
        binding: candidate.binding_digest(),
        subject: owner.subject,
        session: query.session_id,
        principal_epoch: owner.credential.principal_epoch(),
        credential_generation: owner.credential.generation(),
        nonce,
        operation: candidate
            .operation()
            .operation
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        epoch: candidate.epoch().as_u64(),
        candidate: Some(candidate),
        phase: Phase::NeedsStop,
        expires: Instant::now() + PREVIEW_TTL,
        message: "上次电脑操作结果未确认。先停止原执行器，再由你核对外部结果。".into(),
    };
    let view = entry.view();
    *slot = Some(entry);
    Json(view).into_response()
}

async fn retire(State(st): State<LocalState>, Json(decision): Json<Decision>) -> Response {
    decide(st, decision, false).await
}
async fn acknowledge(State(st): State<LocalState>, Json(decision): Json<Decision>) -> Response {
    decide(st, decision, true).await
}
async fn decide(st: LocalState, decision: Decision, reviewed: bool) -> Response {
    let owner = match verified_owner(&st, &decision.session_id).await {
        Ok(v) => v,
        Err(e) => return *e,
    };
    if decision.recovery_id.len() != 64
        || (reviewed && decision.result.as_deref() != Some("reviewed"))
        || (!reviewed && decision.result.is_some())
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut slot = service().entry.lock().await;
    let Some(entry) = slot.as_mut() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let expected = if reviewed {
        Phase::NeedsReview
    } else {
        Phase::NeedsStop
    };
    if !entry.belongs(&owner, &decision.session_id)
        || entry.nonce != decision.recovery_id
        || entry.credential_generation != owner.credential.generation()
        || Instant::now() >= entry.expires
        || entry.phase != expected
    {
        return (
            StatusCode::CONFLICT,
            Json(View::unavailable(
                "确认已失效，请重新打开恢复入口；电脑控制仍暂停",
            )),
        )
            .into_response();
    }
    let Some(candidate) = entry.candidate.as_ref().cloned() else {
        return StatusCode::CONFLICT.into_response();
    };
    let authorization = consent(owner, entry, if reviewed { b"reviewed" } else { b"retire" });
    // 每次接受决定后立即轮换，失败回到相同步骤也不能重放上次 POST。
    entry.nonce = match nonce() {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    entry.expires = Instant::now() + PREVIEW_TTL;
    entry.phase = Phase::Working;
    entry.message = if reviewed {
        "正在核对记录并恢复电脑操作"
    } else {
        "正在停止原执行器"
    }
    .into();
    let binding = entry.binding;
    let view = entry.view();
    let service = Arc::clone(service());
    tokio::spawn(async move {
        let completion = crate::computer_ledger::recovery_completion_probe(&candidate);
        let mut result = if reviewed {
            crate::computer_ledger::complete_recovery(candidate.clone(), authorization).await
        } else {
            crate::computer_ledger::begin_recovery(candidate.clone(), authorization)
                .await
                .map(|()| 0)
        };
        if reviewed
            && result.is_err()
            && let Ok(probe) = completion
            && let Ok(Some(epoch)) = crate::computer_ledger::recovery_completed(&probe).await
        {
            // 只补 UI 应答；原 Actor 已完成结果不签发新的授权或执行任何输入。
            result = Ok(epoch);
        }
        let mut slot = service.entry.lock().await;
        if let Some(entry) = slot.as_mut()
            && entry.binding == binding
            && entry.phase == Phase::Working
        {
            match result {
                Ok(epoch) if reviewed => {
                    entry.phase = Phase::Complete;
                    entry.epoch = epoch;
                    entry.candidate = None;
                    entry.message = "电脑操作已恢复。后续任务需要重新观察窗口。".into();
                }
                Ok(_) => {
                    entry.phase = Phase::NeedsReview;
                    entry.message =
                        "原执行器已停止。请检查目标应用，确认上次操作的外部结果已经核对。".into();
                }
                Err(_) => {
                    // 重试先重新确认停止；原 Actor 已保管证明时不会重复签发或重放输入。
                    entry.phase = Phase::NeedsStop;
                    entry.message = "恢复未完成，电脑控制仍暂停。请重新核对后重试。".into();
                }
            }
        }
    });
    (StatusCode::ACCEPTED, Json(view)).into_response()
}

#[cfg(test)]
#[path = "computer_recovery_test.rs"]
mod tests;
