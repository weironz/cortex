use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use cortex_proto::delegate::Delegation;
use cortex_proto::dto::SessionRuntimeDto;

use super::err;
use crate::delegated_token::DelegatedScope;
use crate::state::AgentState;

// ─────────────────────────── 凭据与容器 ───────────────────────────

/// 这个会话的沙箱作用域：归谁、在哪个项目、该跑在哪儿。
///
/// # 查不到会话行按「未分组」处理，不算错误
///
/// 会话行随第一条 episode 一起建，所以**新会话的第一轮必然查不到**。
/// 一个新会话还没有被移进任何项目，未分组就是正确答案。
///
/// 库连不上也按未分组处理：那时该报错的是紧随其后的那次真正的读写，
/// 不是「决定用哪个容器」这一步 —— 在这儿报，错误信息指向的东西与病因
/// 毫不相干。
async fn delegation_scope(
    st: &AgentState,
    owner: &str,
    session_id: &str,
) -> (DelegatedScope, cortex_store::SessionRuntime) {
    // 项目与执行归属来自**同一行**。分两次查的话，并发改动下两个判断会看到
    // 不同的快照，症状是「偶尔进错容器」—— 没有比这更难复现的
    let state = match st.tenant_for_user(owner).await {
        Ok(tenant) => match tenant.store() {
            Ok(store) => store.session_state(session_id).await.ok().flatten(),
            Err(e) => {
                tracing::debug!(owner, error = %e, "这个部署没有库，沙箱按未分组处理");
                None
            }
        },
        Err(e) => {
            tracing::debug!(owner, error = %e.message(), "租户库够不着，沙箱按未分组处理");
            None
        }
    };
    let runtime = state
        .as_ref()
        .map_or(cortex_store::SessionRuntime::Cloud, |s| s.runtime);
    (
        DelegatedScope {
            owner: owner.to_owned(),
            session_id: session_id.to_owned(),
            project: state.and_then(|s| s.project_id),
        },
        runtime,
    )
}

/// 给这个作用域签一把委托凭据 —— **有就复用，没有才签新的**。
///
/// # 为什么复用而不是每轮签一把
///
/// 令牌的生命周期**跟着容器走**，不跟着轮次走。容器的入站认证用的是它启动时
/// env 里那把（`cortex-local` 的入站与出站共用一个 token）。每轮签新的话，
/// 第二轮反代过去就是 401 —— 而那条错误读起来像「沙箱坏了」。真机上
/// 「第一轮好使、第二轮 401」就是这个。
///
/// 复用的判据是**作用域键**，不是 owner：同一个用户换到另一个项目，那是另一
/// 个容器、另一把令牌。按 owner 找会把 A 项目的令牌塞给 B 项目的容器，
/// 而那个容器起来之后照常应答，只是它认的是另一把，第二轮才 401。
///
/// # 为什么单开一个函数
///
/// 它有两个调用方：这个进程自己那几条碰容器的路由，以及
/// `POST /delegated-tokens`（第三方自己编排沙箱时走的那条）。**两者必须共用
/// 一份实现** —— 签发规则漂开的症状是「从另一条路进来的容器第二轮 401」，
/// 而那两条路服务的是不同的人群。
fn delegate_token(st: &AgentState, scope: DelegatedScope) -> String {
    let key = scope.key();
    // 命中就直接用，**不再改绑**：授权判据是作用域（owner + 项目），
    // 与这把钥匙当初为谁签的无关。改绑那一步从前会把上一个还在跑的那一轮
    // 打死，见 `DelegatedScope::may_write_session`
    match st.delegations().find_by_key(&key) {
        Some(t) => t,
        None => st.delegations().issue(scope),
    }
}

/// 这一步以前是 `st.remote().delegate(...)`，一次跨进程调用，于是每个调用点
/// 都要处理「记忆服务连不上」并回 502。**记忆服务已经不在了**，而签一把钥匙
/// 要的三样东西现在全在本进程：owner 来自凭据，项目与执行归属来自本进程库里
/// 那行会话，额度来自本进程的账本。
///
/// 库够不着时 [`delegation_scope`] 按「未分组」降级（理由见它的文档），
/// 所以这里没有失败可言。给它一个 `Result` 只会在每个调用点留一段永远走不到
/// 的 502 分支 —— 而那种分支会让下一个人以为这条路还在跨进程。
pub(super) async fn issue_delegation(
    st: &AgentState,
    headers: &HeaderMap,
    session_id: &str,
) -> Delegation {
    let owner = crate::accounts::current_user(st, headers).await;
    let (scope, runtime) = delegation_scope(st, &owner, session_id).await;
    let scope_key = scope.key();
    // 额度只**报告**，不在这里拦：这把钥匙不只用于对话，拿它去列文件、下载
    // 工作区一个 token 都不花。用额度拦签发等于用户额度用完之后连自己的文件
    // 都拿不走。见 `Delegation::quota_exhausted`
    let quota_exhausted = st.enforce_quota(&owner).await.is_err();
    Delegation {
        token: delegate_token(st, scope),
        owner,
        scope_key,
        runtime: match runtime {
            cortex_store::SessionRuntime::Local => SessionRuntimeDto::Local,
            cortex_store::SessionRuntime::Cloud => SessionRuntimeDto::Cloud,
        },
        quota_exhausted,
    }
}

/// `POST /delegated-tokens` —— 给调用方签一把绑在某个会话上的钥匙。
///
/// 见 [`cortex_proto::delegate`] 的模块头：这是**入站授权**，不是沙箱编排。
/// owner 来自凭据，项目与执行归属来自会话行，三样一次给全。
pub(super) async fn delegate(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<cortex_proto::delegate::DelegateRequest>,
) -> Json<Delegation> {
    Json(issue_delegation(&st, &headers, &req.session_id).await)
}

/// `DELETE /delegated-tokens` —— 收回一把用不上了的钥匙。
///
/// 幂等：作废一把不存在的（伪造的、已过期的、别人的）同样回 204。
/// 分开报会把「猜一把试试」变成一个可用的探针。
pub(super) async fn revoke_delegation(
    State(st): State<AgentState>,
    Json(req): Json<cortex_proto::delegate::RevokeRequest>,
) -> StatusCode {
    st.delegations().revoke(&req.token);
    StatusCode::NO_CONTENT
}

/// 拿到一个能干活的沙箱：签一把钥匙，再确保容器在跑。
///
/// # 为什么每一条要摸容器的路由都得走它
///
/// 上一版只有 `/chat` 会拉容器，文件那几条是「容器不在就报错」。于是用户从
/// 侧栏点开文件树，看到的是一句「沙箱容器不在了，去发条消息把它拉起来」——
/// 一件他既不该知道也无法理解的事。现在谁先来谁负责拉起，冷启动 913 ms。
///
/// # 起不来时**不作废那把钥匙**
///
/// 这里以前有一句 `remote().revoke()`，理由写的是「容器没起来，它就是一把
/// 没有主人的钥匙」。那句话在「每轮签一把新的」时成立，而
/// [`delegate_token`] 是**复用**的：作废掉的很可能是一把某个活着的容器正在
/// 认的令牌。
///
/// 后果是具体的：`ensure` 最常见的失败是就绪超时，而 `runner::ensure` 的文档
/// 明写着「容器还在……**重试会复用它**」——复用的前提正是下一次拿到同一把
/// 令牌（`token_matches` 才判真）。在这里作废，等于把那句承诺换成「每次就绪
/// 超时都把容器连同里面跑着的东西强制重建一次」。
///
/// 不作废的代价只有一条过期前占位的记录，而 [`crate::delegated_token`] 的
/// TTL 会清掉它。
pub(super) async fn ensure_sandbox(
    st: &AgentState,
    headers: &HeaderMap,
    session_id: &str,
) -> Result<(Delegation, crate::runner::SandboxHandle), Box<Response>> {
    let d = issue_delegation(st, headers, session_id).await;
    match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            // 只有**真的进了容器**才算用过一次。见 `AgentState::last_use`
            st.touch(&d.scope_key);
            Ok((d, h))
        }
        // ⚠️ **装箱**：`Response` 有 128 字节，而这个 `Result` 在六个调用点上
        // 按值传。不装的话每一次**成功**的调用也要为那个失败分支腾出同样大的
        // 栈空间 —— clippy 的 `result_large_err` 说的就是这件事
        // （Rust 1.98 把它打开在这条上）。
        Err(e) => Err(Box::new(err(StatusCode::BAD_GATEWAY, &e.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::router;
    use crate::routes::tests::{NeverRunning, topology_state};
    use axum::body::Body;
    use axum::http::{Method, Request, header};
    use tower::ServiceExt as _;

    /// **委托凭据能进来，但只进得了它够得着的那几条。**
    ///
    /// 这条走的是完整的一圈：签一把 → 拿它打真实的 `router()` → 看那道门。
    /// `auth.rs` 里 `a_logged_in_access_token_is_not_the_preshared_one` 那条
    /// 记着的教训就是这个 —— 账号体系当初断在「注册、登录、拿到 token，然后
    /// 每个请求 401」，而三类测试没有一条**从签发走到用它**。
    ///
    /// 两个方向都要钉：够得着的路由不能 401（否则容器一句话都写不了），
    /// 够不着的必须 403 而不是 401 —— 「凭据不对」与「这把钥匙开不了这扇门」
    /// 是两件事，混成一个码会让排查从看日志变成猜。
    #[tokio::test]
    async fn a_delegated_credential_is_admitted_only_where_its_scope_reaches() {
        // 认证**真的开着**：关掉它这一整条断言就是空的（`Disabled` 一路放行）。
        // 明文那把 token 用不上 —— 被测的是另一把钥匙
        let (_, digest_hex) = crate::auth::generate();
        let raw = hex::decode(&digest_hex).expect("生成的摘要应当是合法十六进制");
        let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            None,
            None,
            None,
            crate::auth::AuthMode::Token { digest },
            None,
        );
        let delegated = st.delegations().issue(DelegatedScope {
            owner: "01OWNER".into(),
            session_id: "01SESSION".into(),
            project: None,
        });
        let app = router(st);

        let status = |method: Method, uri: &'static str, body: &'static str| {
            let app = app.clone();
            let delegated = delegated.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {delegated}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible")
                .status()
            }
        };

        // 够得着：`/llm/stream` 在白名单里。这个部署没配模型，所以它会 501 ——
        // 而 501 恰恰证明请求**穿过了那道门**走进了 handler
        let reachable = status(
            Method::POST,
            "/llm/stream",
            r#"{"tier":"main","system":"","messages":[],"tools":[]}"#,
        )
        .await;
        assert_eq!(
            reachable,
            StatusCode::NOT_IMPLEMENTED,
            "白名单里的路由拿委托令牌打过去得到 {reachable}。401 说明这道门根本不认委托令牌，\
             403 说明白名单漏了它 —— 两种都会让容器里的 agent 一句话都发不出来"
        );

        // 够不着：`/projects` 不在白名单里。**403 而不是 401**
        let out_of_scope = status(Method::GET, "/projects", "").await;
        assert_eq!(
            out_of_scope,
            StatusCode::FORBIDDEN,
            "作用域外的路由得到 {out_of_scope}，预期 403。200 说明作用域根本没被检查，\
             一把沙箱钥匙就此等同于用户的完整凭据"
        );

        // 够得着：取技能正文那条。这个部署没有数据库，所以它会 501 ——
        // 而 501 恰恰证明请求穿过了那道门。
        //
        // ⚠️ 这条**是实测补上的**：第一版漏了它，症状是模型在云端会话里调
        // `load_skill` 连吃两个 403，然后老实告诉用户「取不回来」—— 桌面端
        // 却好使，而两边跑的是同一份 agent 代码，看起来完全像随机故障
        let skill_body = status(Method::GET, "/skills/body?name=x", "").await;
        assert_eq!(
            skill_body,
            StatusCode::NOT_IMPLEMENTED,
            concat!(
                "取技能正文那条拿委托令牌打过去得到 {skill_body}。403 说明白名单漏了它 —— ",
                "云端会话里的技能会静默地全部取不回来，而桌面端一切正常",
            ),
            skill_body = skill_body,
        );

        // 够不着：整张技能表（含所有正文）是**设置页**的路。容器里没有任何
        // 理由把全部正文一次拉走 —— 那是把「按需取回」这层设计绕过去了
        let whole_table = status(Method::GET, "/skills", "").await;
        assert_eq!(
            whole_table,
            StatusCode::FORBIDDEN,
            concat!(
                "沙箱拿委托令牌拉整张技能表得到 {whole_table}，预期 403 —— ",
                "放行它等于让不可信代码一次拿走所有正文，而分层的意义正是不这么做",
            ),
            whole_table = whole_table,
        );

        // 够不着，而且是最要紧的那一条：`/confirmations` 放行会让 agentd 把
        // 请求原样转回同一个容器，一个无限回环
        let loopback = status(Method::GET, "/confirmations", "").await;
        assert_eq!(
            loopback,
            StatusCode::FORBIDDEN,
            "沙箱拿委托令牌打 /confirmations 得到 {loopback}，预期 403 —— \
             这条路在 agentd 上是「原样转进容器」，放行它就是容器问自己"
        );
    }

    /// **同一个作用域两次要钥匙，拿到的必须是同一把。**
    ///
    /// 走的是真实路由（`POST /delegated-tokens`），所以它钉的是
    /// `delegate_token` 真的接在这条端点上，而不只是那个函数自己的性质
    /// （那一半由 `delegated_token::the_same_scope_gets_the_same_token_back`
    /// 守）。
    ///
    /// 红了的症状是真机上「第一轮好使、第二轮 401」：容器的入站认证认的是它
    /// 启动时 env 里那把，而我们每轮换了新的。
    #[tokio::test]
    async fn asking_twice_for_the_same_scope_returns_the_same_key() {
        let app = router(topology_state());
        let ask = |session: &'static str| {
            let app = app.clone();
            async move {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri("/delegated-tokens")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(format!(r#"{{"session_id":"{session}"}}"#)))
                            .expect("构造请求不该失败"),
                    )
                    .await
                    .expect("router 的错误类型是 Infallible");
                assert_eq!(resp.status(), StatusCode::OK, "签钥匙这条路本身不该失败");
                let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                    .await
                    .expect("读 body");
                serde_json::from_slice::<Delegation>(&bytes).expect("回执要是一个 Delegation")
            }
        };

        let first = ask("S1").await;
        let second = ask("S2").await;
        assert_eq!(
            first.token, second.token,
            "同一个作用域第二次拿到了另一把钥匙 —— 容器认的是启动时那把，\
             于是第二轮反代进去就是 401，而那条错误读起来像「沙箱坏了」"
        );
        assert_eq!(
            first.scope_key, second.scope_key,
            "两次的作用域名必须一致，否则会去找一个不存在的容器、于是每轮新建一个"
        );
        assert!(
            matches!(first.runtime, SessionRuntimeDto::Cloud),
            "查不到会话行时执行归属该默认 Cloud —— 默认成 Local 的话，\
             每个新会话的第一轮都会被 /chat 以 409 拒掉"
        );
    }
}
