//! 在线名册 —— 「哪台机器上的 agent 现在活着，以及它手上有哪些会话的绑定」。
//!
//! # 这一步**只报，不改路由**
//!
//! roadmap 的 E 条分四步走，这是第三步。它买到的东西很窄但很实：
//! **「哪台机器」第一次成为用户看得见的信息**，而授权规则一个字没改。
//!
//! 在这之前，一个绑在某台机器上的会话在 Web 上会被顶回来，而那句话只能说
//! 「请到那台机器上打开它」——**没说是哪一台，也没说它现在开着没有**。
//! 用户要么记得，要么挨个试。有了名册之后同一个 409 变成「绑在 willoptpc
//! 上，它现在在线」。
//!
//! # 为什么现在就能上，不必等中继
//!
//! 因为它**不参与任何判断**。报上来的东西只用于两件事：拼一句给人看的话、
//! 以及 `GET /agents` 那个列表。哪怕一个 agent 谎报自己持有某个会话的绑定，
//! 后果也只是那句话说错了 —— 它不会因此拿到任何数据，也不会有哪一轮对话被
//! 路由过去（那是第四步的事，而第四步的判据仍然是「它拿不拿得出那份绑定」）。
//!
//! 这条性质是这一步能独立发布的**全部**理由，所以写在最前面。
//!
//! # 机器标识是**提示**，不是身份
//!
//! `machine_hint` 只用来找人与拼文案。`cortex-store` 里那条论证不变：
//! 本机绑定存在那台机器的 `workspaces.json` 里，**「这台机器有没有这个会话
//! 的绑定」本身就是设备检查** —— 比一个 id 更硬，id 在重装或克隆之后会骗人。
//!
//! 所以别把 `machine_hint` 拿去做任何判断。它是个字符串，用户拿它认机器。

use serde::{Deserialize, Serialize};

/// 本地 agent 定期打上来的一条心跳。
///
/// # 为什么带上会话列表而不是只带一个「我活着」
///
/// 「我活着」答不了用户真正的问题。他问的是「我那个绑在别处的会话该去哪台
/// 机器上打开」，而回答它需要知道**哪台机器持有哪些绑定**。
///
/// 列表不会很长：它是这台机器上真的绑过目录的会话数，而绑定是用户一次一次
/// 点出来的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    /// 这个 agent 进程的稳定标识。
    ///
    /// 同一台机器重启 agent 之后**可以换**：名册按 (owner, agent_id) 存，
    /// 旧的那条会因为不再有心跳而在 TTL 之后自然消失。
    pub agent_id: String,
    /// 给人看的机器名（通常是 hostname）。**不做任何判断**，见模块文档。
    pub machine_hint: String,
    /// 这台机器上绑过本机目录的会话 id。
    ///
    /// 空数组是合法且常见的：一个刚起来、用户还没选过目录的 agent。
    #[serde(default)]
    pub sessions: Vec<String>,
    /// **这台机器同意被远程接入**时带上它，否则 `None`。
    ///
    /// 默认 `None` —— 让云端够到你笔记本上那个能跑 shell 的进程，必须是机器
    /// 主人的一次显式决定（`cortex-local --allow-remote-attach`），
    /// 不能是「装上就有」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<AttachOffer>,
}

/// 「你可以从这个地址接进来，用这把钥匙」。
///
/// # 这把钥匙**不是**入站凭据
///
/// 本地 agent 的入站凭据（`inbound_token`）能做的事包括换出站凭据、绑工作区、
/// 改 MCP 配置 —— 那是「拉起我的那个桌面端」的权限。把它交给云端等于把笔记本
/// 的钥匙给出去。
///
/// 这里给的是一把**另铸的**、只在接入面上有效的钥匙：`POST /chat`、
/// `GET /runs/{id}`、`POST /confirmations`、`/health`。其余一律 401 ——
/// 白名单 + 默认拒绝，与沙箱那侧的委托令牌同一个形状，只是镜像到本地。
///
/// # 它只在心跳里出现，不在任何响应里
///
/// 服务端把它留在内存的名册里，**从不下发给客户端**（`AgentPresenceDto` 只
/// 有一个布尔）。下发它等于让任何一个登录过的设备都能直连别人的笔记本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachOffer {
    /// agentd 可以**直拨**的地址。`None` = 只能经反向隧道够到我。
    ///
    /// # 为什么可以是 `None`
    ///
    /// 反向隧道（2026-08-27）之前这里必须有值，而 worker 在没有
    /// `--attach-addr` 时报的是自己**实际绑到**的那个地址 —— 桌面端绑的正是
    /// `127.0.0.1:x`，云端打过去是打到它自己身上。当时靠「`--allow-remote-attach`
    /// 与 loopback 绑定互斥、启动即拒」堵住。
    ///
    /// 有了隧道之后那条互斥被放宽（可达性来自出站连接），于是**必须让这里
    /// 能说「我没有可直拨的地址」**。继续报 `127.0.0.1:x` 的后果不是安全
    /// 问题而是**诊断说谎**：隧道断掉之后，agentd 探不通那个地址，于是那句
    /// 409 会说「查一下它 --bind 的地址是不是够得到的」，而真相是那台机器
    /// 休眠了。实测撞到过。
    ///
    /// 只有用户**显式**给了 `--attach-addr` 时这里才有值 —— 那是他明确说
    /// 「云端请拨这个」，而他比代码更清楚自己的拓扑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// 接入面专用的钥匙。见结构体文档。
    pub token: String,
}

/// 心跳的回执。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatAck {
    /// 服务端认为这条心跳多久之后过期。
    ///
    /// **由服务端说而不是客户端猜**：两侧各写一个常数的话，改一边就会出现
    /// 「agent 以为自己还在线、名册里已经没了」——而那正是用户看到
    /// 「机器离线」但机器明明开着的那种困惑。
    pub ttl_secs: u64,
}

/// 名册里的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPresenceDto {
    pub agent_id: String,
    pub machine_hint: String,
    /// 距上一条心跳多少秒。客户端据此显示「刚刚」/「30 秒前」。
    pub last_seen_secs: u64,
    /// 这台机器报告自己持有多少个会话的绑定。
    ///
    /// 只给个数不给 id 列表：列表是拿来回答「这个会话在哪」的，
    /// 而那个问题由 [`AgentPresenceDto::has_session`] 那条路径答（见
    /// `GET /agents?session=`）。整份 id 列表下发只会让一个诊断接口
    /// 变成一次会话清单泄露给别的设备。
    pub session_count: usize,
    /// 查询里带了 `?session=` 时：这台机器有没有那个会话的绑定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_session: Option<bool>,
    /// 这台机器同意被远程接入，**而且服务端刚才真的探通了**。
    ///
    /// 只是个布尔：地址与钥匙都不下发。见 [`AttachOffer`]。
    ///
    /// 「同意」与「探通了」合成一个字段是刻意的：分成两个的话，客户端要自己
    /// 判断「同意但探不通」该显示什么，而那个判断迟早与服务端的漂开。
    #[serde(default)]
    pub attachable: bool,
}

/// `GET /agents` 的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsResponse {
    /// 还在 TTL 内的 agent。**过期的不出现**，而不是带一个 `online: false` ——
    /// 「不在名册里」与「在名册里但离线」是同一件事，两种表达会让客户端多一条
    /// 分支，而那条分支迟早与服务端的判断漂开。
    pub agents: Vec<AgentPresenceDto>,
}

/// `GET /agents` 的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentsQuery {
    /// 只关心某个会话时带上它 —— 每行会多一个 `has_session`。
    pub session: Option<String>,
}
