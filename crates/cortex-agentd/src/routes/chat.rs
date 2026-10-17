use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use cortex_proto::dto::{ChatRequest, SessionRuntimeDto};

use super::delegate::issue_delegation;
use super::{MAX_CHAT_BODY, err, err_deterministic};
use crate::state::AgentState;

// ──────────────────────────── /chat ────────────────────────────

/// 一轮对话。**这个 handler 一行对话业务都不做。**
///
/// 起容器（幂等）、拿一把作用域令牌、把整条请求原样反代进去。对话、工具、
/// 确认全在容器里那个 `cortex-local` 上跑 —— 那是同一个二进制，
/// 只是 `--exec-env=container`。
pub(super) async fn chat(State(st): State<AgentState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_CHAT_BODY).await {
        Ok(b) => b,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("读请求体失败：{e}")),
    };
    let parsed: ChatRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("请求体不是合法的 ChatRequest：{e}"),
            );
        }
    };
    let d = issue_delegation(&st, &parts.headers, &parsed.session_id).await;

    // ── 额度用完了，别白起一个容器 ──
    //
    // 这只是**省一次冷启动**。真正的闸门在 `/llm/stream` —— 钱是在那儿花的，
    // 而那一条不依赖这里的自觉。
    // 见 `cortex_proto::delegate::Delegation::quota_exhausted`
    if d.quota_exhausted {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "这个账号的模型额度已经用完了。文件与历史照常可用。",
        );
    }

    // ── 钉在某台机器上的会话，这儿跑不了 ──
    //
    // 它的文件在那台机器的一个本机目录里，而这里只有云端那个卷。硬跑的话
    // agent 会拿到一个**完全不同的文件系统**：历史里那些路径全都不存在，
    // 而它读不到时说的是「没有这个文件」—— 一句听起来像它失忆了的实话。
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        // ── 那台机器同意被接入？那就把这一轮接过去 ──
        //
        // roadmap E 的阶段 4。**判据不是「它在线」而是三件事同时成立**：
        //
        //   1. 它报告持有这个会话的绑定 —— 也就是它拿得出那份绑定
        //   2. 机器主人显式开了远程接入（`--allow-remote-attach`，默认关）
        //   3. 服务端刚才真的探通了它报的地址
        //
        // 少任何一件就落到下面那句 409。第 2 件是**产品决定不是技术判断**：
        // 让云端够到一个能跑 shell 的进程，必须是机器主人的一次显式选择。
        //
        // 复用 `sandbox_proxy::forward` 而不是另写一份：它已经把「剥掉一切
        // 凭据再换上这条路自己的钥匙」做对了，而那一处正是安全边界。
        // 抄第二份的后果是两处剥离规则漂开，且漂开的那一天没人看得出来。
        let route = st
            .presence()
            .attach_route(&d.owner, &parsed.session_id, &|id| {
                st.tunnels().is_live(&d.owner, id)
            });
        if let Some(route) = route {
            let mut proxied = Request::new(axum::body::Body::from(bytes));
            *proxied.method_mut() = axum::http::Method::POST;
            *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
            proxied.headers_mut().insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            if let Some(resp) = attach_forward(&st, &d.owner, &route, proxied).await {
                return resp;
            }
            // 走到这儿 = 隧道在「取句柄」与「开流」之间断了，且没有直拨可回落。
            // 落进下面那句 409 —— 它会说清在哪一台、该怎么办
        }

        // ── 接不过去：说出**是哪一台**，以及为什么接不了 ──
        //
        // 这句话原来只说「请到那台机器上打开它」—— 没说哪一台，也没说它此刻
        // 在不在线。在线名册（阶段 3）让它答得出来；阶段 4 之后还要答得出
        // 「在线但没开放接入」与「在线且开放了但探不通」的区别，
        // 否则用户唯一能做的还是挨个试。
        // 逐行 `concat!`，而不是靠反斜杠续行拼一句长文案。
        //
        // Rust 的续行**本来是对的**（实测：它会吃掉换行与下一行的缩进）。
        // 换掉它的理由是这段文案第一版是**脚本生成进文件**的，而那个续行没能
        // 正确落地 —— 结果服务端真的吐出一句中间夹着 17 个空格的话，
        // 在 dev 上打出来才看见。
        //
        // `concat!` 加显式换行不依赖源文件怎么写，于是「这句话长什么样」
        // 只由这几行本身决定。用户可见的文案值得这个。
        let msg = attach_failure_message(
            st.presence()
                .why_not_attachable(&d.owner, &parsed.session_id),
            st.presence().rebuilding(),
        );
        // ⚠️ **确定性失败**：那台机器不上线之前，重发一万次是同一句话。
        // 而真正的出路（把它唤醒 / 在它上面开 agent / 换掉这条会话的工作区
        // 绑定）没有一件做得到在这个屏幕上 —— 所以这里明说「别给按钮」，
        // 上面那三句话本身就是全部的出路
        return err_deterministic(StatusCode::CONFLICT, &msg);
    }

    // **不走 `ensure_sandbox`**：那个函数自己要一次钥匙，而这一条上面已经要过
    // 了（额度与执行归属两道闸门都要读它）。再要一次只是多一次库查询 ——
    // 拿到的是同一把令牌，因为 `delegate_token` 是复用的。
    //
    // 起不来时同样**不作废那把钥匙**，理由见 `ensure_sandbox` 的文档
    let handle = match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            st.touch(&d.scope_key);
            h
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    tracing::debug!(owner = %d.owner, scope = %d.scope_key, sandbox = %handle.name, "本轮走云沙箱");

    // 重新组一个请求转进去。**只带 body 与必要的首部** ——
    // `sandbox_proxy::forward` 会把一切凭据剥掉再换上沙箱令牌
    let mut proxied = Request::new(axum::body::Body::from(bytes));
    *proxied.method_mut() = axum::http::Method::POST;
    *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
    proxied.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        crate::sandbox_proxy::UpstreamKind::Container,
        proxied,
    )
    .await
}

/// 重挂不到时那句话。**提成常量是为了让测试能认出它。**
///
/// 容器不在与「这条路由压根没挂上」在状态码上都是 404，所以
/// `the_attach_route_is_actually_mounted` 只能靠正文分辨这两者。
const ATTACH_NO_RUN: &str = "这个会话现在没有正在跑的轮次。";

/// 那一轮接不过去时对用户说的话。**按 [`crate::presence::WhyNot`] 分三档
/// 加一个「压根不在线」**，因为用户该做的事完全不同。
///
/// # ⚠️ 这几句里不许有 markdown
///
/// 它们最终落在客户端 `_ErrorNote` 那个红框里，而那个框用的是裸 `Text` ——
/// `**强调**` 会带着星号原样显示，`` `--flag` `` 会带着反引号。
/// 2026-08-27 实测：这几句原本按 markdown 写，屏幕上是
/// 「那台机器**在线**，但它没有开放远程接入」。
///
/// 修在源头而不是让客户端去渲染：这几句的读者不只有 Flutter（CLI 直接
/// 打印，将来还有别的客户端），而「谁渲染」是每个客户端自己的事 ——
/// 「这句话长什么样」不该取决于它。有测试钉着。
///
/// # 也不许把用户支使去敲命令
///
/// 这里曾经写着「`GET /agents` 能看到当前在线的机器」—— 一句**在错误提示里
/// 教用户发 HTTP 请求**的话。它之所以写得出来，是因为界面上没有那个东西；
/// 而正确的修法是把出路说成用户在**界面上或那台机器上**能做的动作。
///
/// 提成自由函数只为一件事：让测试读得到这几句话。留在 handler 里的话，
/// 要断言它们就得起一个带库、带名册、带认证的服务端 —— 那种测试没人会写。
fn attach_failure_message(
    why: Option<(String, crate::presence::WhyNot)>,
    rebuilding: bool,
) -> String {
    match why {
        // 在线，但机器主人没开远程接入。**这不是故障** —— 所以话要说成
        // 「它没同意」而不是「连不上」，否则用户会去查网络
        Some((machine, crate::presence::WhyNot::NotOffered)) => format!(
            concat!(
                "这个会话绑在 {machine} 上的一个目录里，它的文件只在那儿。\n",
                "那台机器在线，但没有开放远程接入。\n",
                "要在这里继续聊：到那台机器上，用 --allow-remote-attach 重新起 agent；\n",
                "或者直接在它上面打开这个会话。"
            ),
            machine = machine
        ),
        // 开放了、给了直拨地址，却打不通。**地址的问题** —— 这条要把
        // 「它同意了」说在前面，否则用户会以为自己没开对开关
        Some((machine, crate::presence::WhyNot::TunnelDown)) => format!(
            concat!(
                "这个会话绑在 {machine} 上的一个目录里。\n",
                "那台机器刚才还连着，现在断开了 —— 多半是它休眠、关机或断网了。\n",
                "不用改任何设置：它一回来，这个会话就能接着聊。"
            ),
            machine = machine
        ),
        // ⚠️ **名册刚重建时不许说「没有」。**
        //
        // 名册是纯内存的（见 `presence` 模块头），agentd 一重启就是空的，
        // 而一台开着的机器最迟 30 秒才会重新报到。这个窗口里说「没有任何
        // 在线的 agent」等于**每次发版都告诉所有人他们的机器关着**，
        // 而它们一直开着。
        //
        // 这一支必须排在下面那个 `None` 之前 —— match 从上往下匹配，
        // 排反了它永远走不到。
        None if rebuilding => concat!(
            "服务端刚重启，在线名册还在重建（最多半分钟）。\n",
            "这个会话绑在你的一台机器上 —— 稍等一下再试，不用改任何设置。"
        )
        .to_owned(),
        None => concat!(
            "这个会话绑在某台机器上的一个目录里，它的文件只在那儿。\n",
            "而现在没有任何在线的 agent 报告持有它的绑定 —— ",
            "那台机器多半没开，或者上面没在跑 Cortex。\n",
            "把它打开、并确认上面的 Cortex 在跑，这个会话就会回来。"
        )
        .to_owned(),
    }
}

/// 把一个已组好的请求经**反向隧道**送到 `route` 指的那台机器。
///
/// 回 `None` = 隧道刚断（名册里还在 TTL 内，而那条连接已经没了）——
/// 调用方落回自己的「接不了」分支。
///
/// # 这里曾经有一条直拨兜底
///
/// 灰度期（隧道 2026-08-27 随 v0.1.25 发出去）老 worker 不会拨隧道，所以
/// 留了一条「按心跳里报的地址直接打过去」的回退路，并配了一个量具去回答
/// 「还能不能撤」。2026-08-30 撤掉了，判据是三条一起成立：
///
/// - `cortex_auth.attach_route_stats` **一行都没有** —— 那张表零计数时
///   主动不写，所以空表就是「没人接入过」
/// - 生产上远程接入**一次都没被启用过**（两个 agent 都没带
///   `--allow-remote-attach`），而桌面端本来就从不传 `--attach-addr`，
///   于是它**根本产生不出**直拨路
/// - 隧道那条有全链路测试盯着（`tunnel::tests::全链路_…`），每次构建都跑
///
/// 撤掉它买到的不是「少一条用户在走的路」，而是**少一条从没被人走过的
/// 回退路** —— 这个仓库反复吃亏的形状正是「只在故障路径上跑的代码，
/// 没人验就等于没写」：真到隧道断掉那天，那条路多半也是坏的。
pub(super) async fn attach_forward(
    st: &AgentState,
    owner: &str,
    route: &crate::presence::AttachRoute,
    req: Request,
) -> Option<Response> {
    let handle = st.tunnels().get(owner, &route.agent_id)?;
    tracing::debug!(agent = %route.agent_id, "经反向隧道接到那台机器");
    Some(crate::sandbox_proxy::forward_tunneled(handle, req).await)
}

/// `GET /sandbox/runs/{session_id}` —— 挂上容器里那一轮。
///
/// # 为什么用 `status` 而不是 `ensure`
///
/// 其余每条路由都走 [`crate::routes::delegate::ensure_sandbox`]（谁先来谁负责拉起，见它的文档），
/// **这一条刻意相反**：重挂问的是「刚才那件事还在跑吗」，而容器都没了
/// 就说明它不在跑了。`ensure` 会为了回答「没有」而先花 900ms 起一个空容器，
/// 然后立刻被空闲回收 —— 用户每打开一个旧会话就白起一次。
///
/// 所以这条不拉起、不签新钥匙，容器不在就直接 404。
///
/// # 404 不是错误
///
/// 绝大多数会话此刻都没在跑。客户端按「照常拉历史」处理 —— 与桌面端
/// 那条 `/runs/{id}` 回的是同一个状态码，两端的判断因此是同一份。
pub(super) async fn attach_run(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    req: Request,
) -> Response {
    // 钥匙还是要要一次：`scope_key` 由 owner 与项目派生，而这条路由要拿它去问
    // 「那个容器在不在」。**没派上用场也不作废**，理由见 `ensure_sandbox`
    let d = issue_delegation(&st, &headers, &session_id).await;

    // ── 钉在某台机器上的会话：这一轮（若在跑）在**那台机器**上 ──
    //
    // 此前这条只查容器，于是 web 端刷新页面想重挂桌面上正在跑的一轮，
    // 拿到的是 404「没有正在跑的轮次」，而桌面明明在跑（对抗评审点名的
    // 阶段 1 前置洞）。机器够不着时仍回 404 —— 客户端把它当「拉历史」
    // 处理，与「轮次真的不在跑」同一条路径，这正是想要的降级
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        let route = st.presence().attach_route(&d.owner, &session_id, &|id| {
            st.tunnels().is_live(&d.owner, id)
        });
        if let Some(route) = route {
            let (parts, _) = req.into_parts();
            let method = parts.method.clone();
            let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
            *proxied.method_mut() = method;
            *proxied.uri_mut() = format!("/runs/{session_id}")
                .parse()
                .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));
            if let Some(resp) = attach_forward(&st, &d.owner, &route, proxied).await {
                return resp;
            }
        }
        return err(StatusCode::NOT_FOUND, ATTACH_NO_RUN);
    }

    let handle = match st.runner().status(&d.scope_key).await {
        Ok(Some(h)) => h,
        // 容器不在 = 那一轮不在跑
        Ok(None) => return err(StatusCode::NOT_FOUND, ATTACH_NO_RUN),
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    st.touch(&d.scope_key);

    // 重组一个只带必要首部的请求。`sandbox_proxy::forward` 会把一切凭据
    // 剥掉再换上沙箱令牌 —— 用户的 bearer 绝不进容器
    let (parts, _) = req.into_parts();
    // **保留进来的方法**：GET 是重挂，DELETE 是掐掉那一轮。两者除了方法之外
    // 一模一样（都要先确认容器在、都不 `ensure`），所以共用这一个 handler
    // 而不是抄一份 —— 抄一份的下场是「404 的条件」两处慢慢漂开
    let method = parts.method.clone();
    let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
    *proxied.method_mut() = method;
    *proxied.uri_mut() = format!("/runs/{session_id}")
        .parse()
        .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        crate::sandbox_proxy::UpstreamKind::Container,
        proxied,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::router;
    use crate::routes::tests::topology_state;
    use axum::body::Body;
    use axum::http::Method;
    use tower::ServiceExt as _;

    /// **重挂那条不 `ensure`，`/confirmations` 也不。**
    ///
    /// `NeverRunning::ensure` 一定失败、`status` 一定回 `None`。所以走 `ensure`
    /// 的路由会以 502 结束，而这两条各有自己的「没有容器」出口 ——
    /// 状态码就是「它到底走了哪一条」的证据。
    ///
    /// 这条以前是靠「远端指向一个没人监听的端口，于是要钥匙那一步就 502」
    /// 来证明的。签发 2026-08-16 搬进了本进程，要钥匙不再会失败，于是它
    /// **终于测的是自己文档里写的那件事**。
    ///
    /// 测得的东西看着小，实际拦的是一次真金白银的浪费：走 ensure 的话，用户
    /// 每打开一个旧会话、每答一次确认都会白起一个容器（900ms + 一次卷挂载），
    /// 然后它立刻被空闲回收 —— 而界面上什么异常都看不到。确认那条更糟：
    /// 新容器里根本没有那条待确认记录。
    #[tokio::test]
    async fn attaching_and_answering_never_start_a_container() {
        let app = router(topology_state());
        for (method, uri, want) in [
            (Method::GET, "/sandbox/runs/S1", StatusCode::NOT_FOUND),
            (Method::GET, "/confirmations", StatusCode::CONFLICT),
            (Method::POST, "/confirmations", StatusCode::CONFLICT),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible");
            assert_eq!(
                resp.status(),
                want,
                "{method} {uri} 回了 {} —— 502 说明它走了 `ensure`（替身的 ensure 必失败），\
                 也就是为了回答「没有」而先花 900ms 起了一个空容器",
                resp.status()
            );
        }
    }

    /// 路由**挂上去了**（不是 404 那种「压根没这条路」）。
    ///
    /// 与上面那条一起看：一个是「走对了分支」，一个是「路径拼对了」。
    /// 少了这条的话，把 `{session_id}` 写成 `:session_id`（axum 0.7 的老写法）
    /// 会让这条路由永远 404，而上面那条测试**照样绿**。
    ///
    /// # 为什么必须看正文而不只看状态码
    ///
    /// 「容器不在」与「这条路由没挂上」如今都是 404 —— 前者是这个 handler
    /// 自己给的（见 [`ATTACH_NO_RUN`]），后者是 axum 的兜底。只比状态码的话
    /// 这条测试恒绿，而它要抓的恰恰是后者。
    /// ⚠️ **这几句话里不许有 markdown，也不许支使用户去敲命令。**
    ///
    /// 它们落在客户端 `_ErrorNote` 那个红框里，而那个框用的是**裸 `Text`**
    /// （`message_bubble.dart`）—— `**强调**` 会带着星号原样显示、
    /// `` `--flag` `` 会带着反引号。2026-08-27 实测：屏幕上是
    /// 「那台机器**在线**，但它没有开放远程接入」。
    ///
    /// 修在源头是因为读者不只有 Flutter（CLI 直接打印），
    /// 而「谁渲染」是每个客户端自己的事。
    ///
    /// 第二条断言挡的是另一个形状：那句 `None` 分支曾经写着
    /// 「`GET /agents` 能看到当前在线的机器」—— **在错误提示里教用户发
    /// HTTP 请求**。它写得出来是因为界面上没有那个东西，而那不是用户的错。
    #[test]
    fn the_attach_failure_messages_carry_no_markdown() {
        use crate::presence::WhyNot;
        let cases = [
            Some(("WILLOPTPC".to_owned(), WhyNot::NotOffered)),
            Some(("WILLOPTPC".to_owned(), WhyNot::TunnelDown)),
            None,
        ];
        for why in cases {
            let label = format!("{why:?}");
            let msg = super::attach_failure_message(why, false);

            assert!(
                !msg.contains("**"),
                "{label}：这句话里有 `**` —— 客户端用裸 Text 画它，\
                 用户看到的是带星号的原文：{msg}"
            );
            assert!(!msg.contains('`'), "{label}：这句话里有反引号，同上：{msg}");
            // 支使用户敲 HTTP 请求 = 界面缺东西，而代价被转嫁给了用户
            for bad in ["GET /", "POST /", "curl"] {
                assert!(
                    !msg.contains(bad),
                    "{label}：错误提示里出现了 `{bad}` —— \
                     出路要说成用户在界面上或那台机器上做得到的动作：{msg}"
                );
            }
        }
    }

    /// ⚠️ **名册刚重建时不许说「你的机器都关着」。**
    ///
    /// 名册是纯内存的（见 `presence` 模块头），agentd 一重启就是空的，
    /// 而一台开着的机器最迟 30 秒才会重新报到。这个窗口里说「没有任何
    /// 在线的 agent」等于**每次发版都告诉所有人他们的机器关着** ——
    /// 而它们一直开着。
    #[test]
    fn a_rebuilding_roster_never_claims_the_machines_are_off() {
        let fresh = super::attach_failure_message(None, true);
        assert!(
            fresh.contains("刚重启") && fresh.contains("名册"),
            "窗口内要说清是服务端自己刚起来，不是用户的机器出了事：{fresh}"
        );
        assert!(
            fresh.contains("不用改"),
            "还要说清用户什么都不用做 —— 否则他会去开一台本来就开着的机器：{fresh}"
        );

        // 窗口外仍然要老实说「没有」——否则一台真关着的机器会被永远说成
        // 「稍等一下」，而用户等不到任何东西
        let settled = super::attach_failure_message(None, false);
        assert!(
            settled.contains("没有任何在线的 agent"),
            "窗口外要说实话：{settled}"
        );
        assert_ne!(fresh, settled);
    }

    /// **三档「接不了」说的不能是同一件事。**
    ///
    /// 合成一句的话：没开开关的那台会被当成网络故障（用户去查防火墙），
    /// 而休眠的笔记本会被当成「没开开关」（用户去加一个他早就加了的参数）。
    ///
    /// ⚠️ 这里原来有第三档 `Unreachable`（「去改 `--attach-addr`」），
    /// 2026-08-30 随直拨删掉。
    #[test]
    fn each_attach_failure_reason_says_something_different() {
        use crate::presence::WhyNot;
        let m = |w| super::attach_failure_message(Some(("MBP".to_owned(), w)), false);
        let not_offered = m(WhyNot::NotOffered);
        let tunnel_down = m(WhyNot::TunnelDown);

        // 每一档都要点出是哪一台 —— 不说的话用户只能挨个试
        for msg in [&not_offered, &tunnel_down] {
            assert!(msg.contains("MBP"), "没说是哪一台：{msg}");
        }

        assert!(
            not_offered.contains("--allow-remote-attach"),
            "没开开关那一档要给出开关名，否则用户不知道该做什么：{not_offered}"
        );
        // ⚠️ 最要紧的一条：机器刚离开时**什么都不用改**
        assert!(
            tunnel_down.contains("不用改"),
            "机器刚离开那一档必须说清不用改设置，否则用户会照上一档去查地址：{tunnel_down}"
        );
        assert!(
            !tunnel_down.contains("--attach-addr") && !tunnel_down.contains("--bind"),
            "机器刚离开时提任何地址参数，都是把用户送去改一个没错的东西：{tunnel_down}"
        );

        assert_ne!(not_offered, tunnel_down);
    }

    #[tokio::test]
    async fn the_attach_route_is_actually_mounted() {
        let resp = router(topology_state())
            .oneshot(
                Request::builder()
                    .uri("/sandbox/runs/S1")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains(ATTACH_NO_RUN),
            "404 是 axum 的兜底而不是这个 handler 给的（正文是 {text:?}）—— \
             路径没挂上。客户端会看到「这个会话没在跑」，而它其实在跑"
        );
    }
}
