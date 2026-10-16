use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use cortex_proto::dto::SessionRuntimeDto;

use super::chat::attach_forward;
use super::delegate::issue_delegation;
use super::err;
use crate::state::AgentState;

// ──────────────────────── /confirmations ────────────────────────

/// `/confirmations` 的查询串。
///
/// 字段名与容器那侧的 `PendingQuery` **逐字相同**，因为这条路上的查询串是
/// 原样转进去的（[`crate::sandbox_proxy::forward`] 拿的是完整的
/// `path_and_query`）。在这里改名等于让容器收到一个它不认识的参数，
/// 而 `serde(default)` 会让那件事表现为「筛选没生效」而不是一次报错。
#[derive(serde::Deserialize)]
pub(super) struct ConfirmQuery {
    #[serde(default)]
    session_id: Option<String>,
}

/// `GET|POST /confirmations` —— 反代进容器里那个 `cortex-local`。
///
/// 确认回路的簿子住在容器里（`cortex-local` 的 `confirm.rs`，它是唯一同时
/// 知道「哪一轮在等」与「答案该交给谁」的地方）。这个进程一条确认记录都没有，
/// 所以它只做转发。
///
/// # **不走 [`ensure_sandbox`]**
///
/// 与 [`attach_run`] 同一个理由，而且更强：确认是对一个**已经在跑**的轮次的
/// 回答。为了回答它冷启动一个容器，除了白花 900 ms，那个新容器里根本没有这条
/// 待确认记录 —— 于是用户点「允许」之后拿到的是「没有这条待确认项」，
/// 一句既正确又完全帮不上忙的话。
///
/// # 没有活着的沙箱时回 409，不是 404
///
/// 404 在这条路上**已经有别的意思了**：容器自己用它表示「这个 token 已经用
/// 掉了 / 那一轮超时了」，而客户端据此把这次点击显示成「抢答输了」
/// （`confirm_controller` 的 `ConfirmOutcome.superseded`）。拿 404 表示
/// 「这儿没有沙箱」会把两件补救方式完全不同的事说成同一件。
///
/// 409 会让客户端把正文原样显示出来（`回执没能送达：…`），所以那句话要能
/// 直接读懂、并指出下一步。
///
/// # 不带 `?session_id=` 时落在**未分组**那个沙箱上
///
/// 客户端重连后的那次补拉是不带会话的（它还不知道该关心哪个），于是
/// [`crate::routes::delegate::delegation_scope`] 查不到会话行、按未分组处理 —— 也就是这个用户那个
/// 裸 owner 作用域。项目里的会话要拿到自己那个容器就必须带上
/// `?session_id=`，否则这里会诚实地回 409 而不是去别人的容器里翻。
pub(super) async fn confirmations(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ConfirmQuery>,
    req: Request,
) -> Response {
    let session_id = q.session_id.as_deref().unwrap_or_default();
    let d = issue_delegation(&st, &headers, session_id).await;

    // ── 钉在某台机器上的会话：确认簿在**那台机器**的 cortex-local 里 ──
    //
    // 少了这一支，一个绑在本机目录上的会话在 Web 上永远确认不了：这里会
    // 拿它的 scope 去问「有没有容器」，而那个 scope 下要么没有容器（回一句
    // 指错路的 409「重新发一条消息会把沙箱拉起来」——**桌面上那一轮明明
    // 正在等**），要么恰好有一个别的容器，于是**回一份空的待确认列表**，
    // 而那一轮就在屏幕上无限等下去。
    //
    // ⚠️ 2026-08-27 实测：worker 侧 `GET /confirmations` 拿得到那条待确认，
    // 而经云端同一时刻拿到的是 `{"pending":[]}`。
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        let route = st.presence().attach_route(&d.owner, session_id, &|id| {
            st.tunnels().is_live(&d.owner, id)
        });
        if let Some(route) = route
            && let Some(resp) = attach_forward(&st, &d.owner, &route, req).await
        {
            return resp;
        }
        // 够不着那台机器。**文案要按「机器」说，不按「沙箱」说** ——
        // 容器版那句「重新发一条消息会把沙箱拉起来」在这里是彻头彻尾的
        // 误导：没有沙箱可拉，而重发只会再排一轮等不到确认的对话
        return err(
            StatusCode::CONFLICT,
            "这个会话钉在你的一台机器上，而它现在够不着 —— 确认没能送过去。\n\
             等那台机器重新在线之后再试；这一轮在它上面会因为等不到回答而超时。",
        );
    }

    let handle = match st.runner().status(&d.scope_key).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return err(
                StatusCode::CONFLICT,
                "这个会话现在没有正在跑的沙箱，确认已经无处可送 —— \
                 那一轮多半已经结束或被回收了。重新发一条消息会把沙箱拉起来。",
            );
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    st.touch(&d.scope_key);

    // 请求原样转进去：路径与查询串都不变（容器那侧的路由就叫
    // `/confirmations`），凭据由 `forward` 剥掉再换上沙箱令牌
    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        crate::sandbox_proxy::UpstreamKind::Container,
        req,
    )
    .await
}
