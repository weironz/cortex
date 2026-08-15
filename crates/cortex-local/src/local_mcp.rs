//! MCP server 的增删查改。**设备本地状态**，与工作区绑定同类。
//!
//! # 为什么这几条只有本地 agent 有
//!
//! 配置文件在这台机器上，子进程也在这台机器上跑。cortexd 那侧既没有那个
//! 文件也起不了那个进程 —— 所以 Web 端调这几条会拿到 404，而客户端把它
//! 当成「这个后端没有本机 MCP」，与 `/local/workspace-root` 同一条路数。
//!
//! # 密钥不回传
//!
//! [`ServerView`] 里只有环境变量的**名字**，没有值。MCP server 的 env 常常
//! 就是 API key，而它一旦回到界面，就会出现在截图、录屏、以及任何一次
//! 「帮我看看这个配置」里。
//!
//! 代价是改配置时客户端手上没有旧值，所以 [`upsert`] 做的是**合并**而不是
//! 替换：这次给了值的覆盖，没给的保持原样，要删的走 `remove_env`。

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cortex_mcp::{McpConfig, ServerConfig, ServerStatus, ToolInfo, Transport, Trust};
use serde::{Deserialize, Serialize};

use crate::state::LocalState;

/// 一台 server 在界面上的全部信息：配置 + 现在连没连上。
///
/// 配置与状态**合成一条**而不是让客户端自己 join：两份列表按名字对起来
/// 这件事，客户端做一遍、CLI 再做一遍，而漏掉「配置里有但状态里没有」
/// （刚加还没重连）的那一侧就会让新加的 server 在界面上消失。
#[derive(Debug, Serialize)]
pub struct ServerView {
    pub name: String,
    /// `stdio` 或 `http`。
    pub transport: &'static str,
    /// **将要执行的那条命令**（或 HTTP 的 URL）。
    ///
    /// 服务端算而不是客户端拼：这串东西是「加一台 server = 在这台机器上跑
    /// 任意进程」这件事给用户看的唯一凭据，两处各拼各的就会出现界面显示的
    /// 和真正执行的不是一回事。
    pub command_line: String,
    /// 环境变量的**名字**。值不出这个进程，见模块文档。
    pub env_names: Vec<String>,
    pub trust: Trust,
    pub disabled: bool,
    pub connected: bool,
    pub tools: Vec<ToolInfo>,
    pub error: Option<String>,
}

/// `GET /local/mcp` 的响应。
#[derive(Debug, Serialize)]
pub struct McpView {
    /// 配置文件在哪。界面要能告诉用户「你手编的话编这个」。
    pub path: String,
    pub servers: Vec<ServerView>,
}

/// 把一条配置渲染成人看的命令行。
///
/// `disabled` 的那台也要渲染 —— 用户关掉它之后仍然要看得见它是什么。
#[must_use]
pub fn command_line_of(t: &Transport) -> String {
    match t {
        Transport::Stdio { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {}", args.join(" "))
            }
        }
        Transport::Http { url, .. } => url.clone(),
    }
}

fn view_of(name: &str, sc: &ServerConfig, st: Option<&ServerStatus>) -> ServerView {
    let (transport, env_names) = match &sc.transport {
        Transport::Stdio { env, .. } => ("stdio", env.keys().cloned().collect()),
        // HTTP 那侧的 `headers` 同样可能装着 Authorization，一样只给名字
        Transport::Http { headers, .. } => ("http", headers.keys().cloned().collect()),
    };
    ServerView {
        name: name.to_owned(),
        transport,
        command_line: command_line_of(&sc.transport),
        env_names,
        trust: sc.trust,
        disabled: sc.disabled,
        connected: st.is_some_and(|s| s.connected),
        tools: st.map(|s| s.tools.clone()).unwrap_or_default(),
        error: st.and_then(|s| s.error.clone()),
    }
}

/// 读配置 + 逐台状态。
async fn view(st: &LocalState) -> Result<McpView, cortex_core::CortexError> {
    let path = st.engine.mcp_path.clone();
    let cfg = McpConfig::load(&path)?;
    let statuses = st.engine.mcp.status().await;

    let servers = cfg
        .servers
        .iter()
        .map(|(name, sc)| {
            let s = statuses.iter().find(|s| &s.name == name);
            view_of(name, sc, s)
        })
        .collect();

    Ok(McpView {
        path: path.display().to_string(),
        servers,
    })
}

fn bad(e: &impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, e.to_string()).into_response()
}

/// `GET /local/mcp`
pub async fn list(State(st): State<LocalState>) -> Response {
    match view(&st).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(&e),
    }
}

/// 落盘 + 重连 + 回新视图。三步**必须一起做**。
///
/// 只落盘不重连的话，界面显示「已添加」而模型手上没有那些工具，直到下次
/// 重启 —— 那正是这一整轮要消灭的那种沉默。
async fn commit(st: &LocalState, cfg: &McpConfig) -> Response {
    if let Err(e) = cfg.save(&st.engine.mcp_path) {
        return bad(&e);
    }
    st.engine.mcp.reload(cfg).await;
    match view(st).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(&e),
    }
}

/// `PUT /local/mcp/servers/{name}` 的请求体。
///
/// 不是直接收一个 [`ServerConfig`]：env 要**合并**而不是替换，因为客户端
/// 手上从来没有过旧的值（见模块文档）。
#[derive(Debug, Deserialize)]
pub struct UpsertRequest {
    #[serde(flatten)]
    pub transport: Transport,
    #[serde(default)]
    pub trust: Trust,
    #[serde(default)]
    pub disabled: bool,
    /// 要删掉的环境变量 / 请求头名。
    ///
    /// 「没给值」不能当成「删掉」：那样的话客户端每次改 trust 都会把用户
    /// 填过的 key 全清空，而它并不知道那些 key 长什么样。
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// `PUT /local/mcp/servers/{name}` —— 加一台或改一台。
pub async fn upsert(
    State(st): State<LocalState>,
    Path(name): Path<String>,
    Json(req): Json<UpsertRequest>,
) -> Response {
    if !cortex_mcp::valid_server_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            format!("server 名 {name:?} 不能用：只允许字母、数字、`-`、`_`、`.`，且不能含 `__`"),
        )
            .into_response();
    }

    let mut cfg = match McpConfig::load(&st.engine.mcp_path) {
        Ok(c) => c,
        Err(e) => return bad(&e),
    };

    let mut transport = req.transport;
    merge_secrets(&mut transport, cfg.servers.get(&name), &req.remove_env);

    cfg.servers.insert(
        name,
        ServerConfig {
            transport,
            trust: req.trust,
            disabled: req.disabled,
        },
    );
    commit(&st, &cfg).await
}

/// 把旧的 env / headers 里**这次没提到**的那些带过来。
///
/// 只有这一处知道旧值 —— 客户端拿到的视图里只有名字。
fn merge_secrets(next: &mut Transport, prev: Option<&ServerConfig>, remove: &[String]) {
    let Some(prev) = prev else { return };
    match (next, &prev.transport) {
        (Transport::Stdio { env, .. }, Transport::Stdio { env: old, .. }) => {
            carry(env, old, remove);
        }
        (Transport::Http { headers, .. }, Transport::Http { headers: old, .. }) => {
            carry(headers, old, remove);
        }
        // 换了传输方式：旧的那套 env 对新的没有意义，不带过来
        _ => {}
    }
}

fn carry(
    next: &mut std::collections::BTreeMap<String, String>,
    old: &std::collections::BTreeMap<String, String>,
    remove: &[String],
) {
    for (k, v) in old {
        if !next.contains_key(k) && !remove.iter().any(|r| r == k) {
            next.insert(k.clone(), v.clone());
        }
    }
    for r in remove {
        next.remove(r);
    }
}

/// `DELETE /local/mcp/servers/{name}`
pub async fn remove(State(st): State<LocalState>, Path(name): Path<String>) -> Response {
    let mut cfg = match McpConfig::load(&st.engine.mcp_path) {
        Ok(c) => c,
        Err(e) => return bad(&e),
    };
    if cfg.servers.remove(&name).is_none() {
        return (StatusCode::NOT_FOUND, format!("没有叫 {name} 的 server")).into_response();
    }
    commit(&st, &cfg).await
}

/// `POST /local/mcp/reload` —— 重新读文件、重新连。
///
/// 用户手编过文件之后要有一条路能生效，否则「配置文件在哪」这个信息就是
/// 假的：告诉了他位置，却要他重启才算数。
pub async fn reload(State(st): State<LocalState>) -> Response {
    let cfg = match McpConfig::load(&st.engine.mcp_path) {
        Ok(c) => c,
        Err(e) => return bad(&e),
    };
    st.engine.mcp.reload(&cfg).await;
    match view(&st).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(&e),
    }
}

/// `POST /local/mcp/parse` 的请求体。
#[derive(Debug, Deserialize)]
pub struct ParseRequest {
    pub text: String,
}

/// 解析出来的一台，**还没落盘**。
#[derive(Debug, Serialize)]
pub struct ParsedServer {
    pub name: String,
    pub transport: &'static str,
    pub command_line: String,
    /// 原样的那份配置，客户端确认后原封不动 PUT 回来。
    pub config: ServerConfig,
    /// 这个名字已经被占了。客户端据此提示「会覆盖」。
    pub conflicts: bool,
}

/// `POST /local/mcp/parse` —— 粘一段进来，看看会变成什么。**不落盘**。
///
/// # 为什么解析与落盘是两条路
///
/// 加一台 MCP server = **在这台机器上跑任意进程**。中间必须有一屏让用户
/// 看到那条命令行原文 —— 一次点击就装上的话，他同意的是一个名字，
/// 不是一条命令。
pub async fn parse(State(st): State<LocalState>, Json(req): Json<ParseRequest>) -> Response {
    let parsed = match cortex_mcp::parse_pasted(&req.text) {
        Ok(c) => c,
        Err(e) => return bad(&e),
    };
    let existing = McpConfig::load(&st.engine.mcp_path).unwrap_or_default();

    let out: Vec<ParsedServer> = parsed
        .servers
        .into_iter()
        .map(|(name, sc)| ParsedServer {
            transport: match &sc.transport {
                Transport::Stdio { .. } => "stdio",
                Transport::Http { .. } => "http",
            },
            command_line: command_line_of(&sc.transport),
            conflicts: existing.servers.contains_key(&name),
            name,
            config: sc,
        })
        .collect();
    Json(out).into_response()
}

/// `GET /local/mcp/registry?q=` 的查询串。
#[derive(Debug, Deserialize)]
pub struct RegistryQuery {
    #[serde(default)]
    pub q: String,
}

/// 一页够看了。注册表里同一个词能搜出几百条，而人不会翻到第三屏。
const REGISTRY_LIMIT: u32 = 50;

/// `GET /local/mcp/registry?q=` —— 代查官方注册表。
///
/// 由 agent 代取而不是客户端直连，理由见 `cortex_mcp::registry` 的模块文档：
/// 转换在那边，取数跟着转换走。
pub async fn registry(State(st): State<LocalState>, Query(q): Query<RegistryQuery>) -> Response {
    match cortex_mcp::registry::search(&st.http, &q.q, REGISTRY_LIMIT).await {
        Ok(entries) => Json(entries).into_response(),
        // 502 而不是 400：错的不是请求，是上游够不着（这台机器没网、
        // 或者注册表挂了）。客户端据此说「连不上注册表」而不是「搜索词有问题」
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}
