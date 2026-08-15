//! 连着的那些 MCP server。

use std::collections::BTreeMap;
use std::sync::Arc;

use cortex_agent::{ToolResult, ToolSpec};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::RunningService;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;

use crate::config::{McpConfig, ServerConfig, Transport, Trust};

/// 一台 server 现在什么样。给界面与 `/health` 看。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerStatus {
    pub name: String,
    /// 连上了没有。
    pub connected: bool,
    /// 它提供的工具。连不上时是空的。
    ///
    /// **不另存一个 `count` 字段**：两个必须一致的字段迟早会不一致，
    /// 而那种不一致的症状是界面说「27 个工具」、展开却只有 3 个。
    /// 要数量就 `.len()`。
    pub tools: Vec<ToolInfo>,
    /// 连不上的原因。**连上时为 None** —— 留着旧的错误会让人以为它还坏着。
    pub error: Option<String>,
}

/// 一个外来工具在界面上长什么样。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolInfo {
    /// **加过前缀的全名**（`mcp__server__tool`），不是对端的原名。
    ///
    /// 显示全名而不是剥掉前缀：这一栏回答的是「模型手上有什么」，
    /// 而模型看到的就是这个名字。剥掉之后用户在日志里看到
    /// `mcp__docs__search` 会对不上界面里的 `search`。
    pub name: String,
    pub description: String,
}

/// 一条连接，外加它提供的工具。
struct Server {
    /// `RunningService` 一被 drop，stdio 那条就会收掉子进程。所以这个字段
    /// 存在的意义不只是「用来发请求」——**它同时是子进程的生命周期**。
    service: RunningService<rmcp::RoleClient, ()>,
    specs: Vec<ToolSpec>,
}

/// 所有 MCP 连接。
///
/// # 连不上不是致命错误
///
/// 一台 server 起不来（命令不存在、npx 还在下载、HTTP 端点挂了），
/// **其余的照常工作**，agent 也照常跑 —— 只是少几个工具。
///
/// 反过来做（有一台连不上就拒绝启动）会让一个装了五个插件的人，
/// 因为其中一个的 npm 包临时拉不下来而完全用不了 agent。而 MCP server
/// 大多是别人写的、跑在别人的网络上的东西，把自己的可用性绑在它们的
/// 全部可用上，是把控制权交出去。
///
/// 代价必须可见：[`Self::status`] 逐台报，界面要显示出来 ——
/// 否则「少了几个工具」在用户那儿的表现是「模型今天有点笨」。
pub struct McpHub {
    inner: tokio::sync::RwLock<Inner>,
}

/// 锁里面那份。连接与失败必须一起换，所以它们在同一个结构里。
#[derive(Default)]
struct Inner {
    servers: BTreeMap<String, Server>,
    failures: BTreeMap<String, String>,
}

impl Inner {
    /// 按配置逐台连。
    ///
    /// **串行连而不是并发**：MCP server 多半是 `npx` / `uvx` 拉起来的，
    /// 第一次运行会去下载包。五个一起下，五个都慢，而且日志会交织成
    /// 一团谁也看不懂的东西。串行的代价是首次启动慢几秒，
    /// 换的是「卡在哪一台」一眼可见。
    async fn build(cfg: &McpConfig) -> Self {
        let mut inner = Self::default();
        for (name, sc) in cfg.enabled() {
            match connect_one(name, sc).await {
                Ok(server) => {
                    tracing::info!(
                        server = name,
                        tools = server.specs.len(),
                        "MCP server 已连接"
                    );
                    inner.servers.insert(name.clone(), server);
                }
                Err(e) => {
                    // warn 不是 error：这是**预期内**的降级，不是故障。
                    tracing::warn!(server = name, error = %e, "MCP server 连不上，跳过它继续");
                    inner.failures.insert(name.clone(), e);
                }
            }
        }
        inner
    }
}

impl McpHub {
    /// 空的。没有配置文件、或者配置里一台都没开时就是它。
    #[must_use]
    pub fn empty() -> Self {
        Self {
            inner: tokio::sync::RwLock::new(Inner::default()),
        }
    }

    /// 按配置逐台连。见 [`Inner::build`]。
    pub async fn connect(cfg: &McpConfig) -> Self {
        Self {
            inner: tokio::sync::RwLock::new(Inner::build(cfg).await),
        }
    }

    /// 按新配置**整体**重连，回新的逐台状态。
    ///
    /// # 为什么不做增量 diff
    ///
    /// 「只重连改动的那台」看着省事，代价是内存里出现第二份账本：谁在连、
    /// 连的是哪一版配置。两份账本分叉的症状是**界面显示已连接、模型手上
    /// 却没有那个工具** —— 不报错，只是模型「不会用那个功能」。
    ///
    /// 配置文件是唯一真相，整体重建就不可能分叉。代价是改一台会重连全部
    /// （首次拉包时几秒），而这是用户主动点的操作，等得起。
    ///
    /// # 先连新的，再放旧的
    ///
    /// 旧连接一 drop，stdio 那些子进程就被收掉。所以顺序是：**不持锁**
    /// 建好新的（慢的那一步），拿写锁换进去，出锁之后旧的才析构。
    /// 反过来做的话，整个重连期间读锁全被挡住 —— 而正在跑的一轮对话
    /// 恰好要读 `specs()`。
    pub async fn reload(&self, cfg: &McpConfig) -> Vec<ServerStatus> {
        let fresh = Inner::build(cfg).await;
        let old = {
            let mut guard = self.inner.write().await;
            std::mem::replace(&mut *guard, fresh)
        };
        drop(old);
        self.status().await
    }

    /// 所有 server 提供的工具，可以直接并进 `Turn::with_specs`。
    pub async fn specs(&self) -> Vec<ToolSpec> {
        self.inner
            .read()
            .await
            .servers
            .values()
            .flat_map(|s| s.specs.iter().cloned())
            .collect()
    }

    /// 逐台的状态。**连不上的也要在里面** —— 那正是要给用户看的。
    pub async fn status(&self) -> Vec<ServerStatus> {
        let inner = self.inner.read().await;
        let mut out: Vec<ServerStatus> = inner
            .servers
            .iter()
            .map(|(name, s)| ServerStatus {
                name: name.clone(),
                connected: true,
                tools: s
                    .specs
                    .iter()
                    .map(|sp| ToolInfo {
                        name: sp.name.to_string(),
                        description: sp.description.to_string(),
                    })
                    .collect(),
                error: None,
            })
            .collect();
        out.extend(inner.failures.iter().map(|(name, e)| ServerStatus {
            name: name.clone(),
            connected: false,
            tools: Vec::new(),
            error: Some(e.clone()),
        }));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// 调一个外来工具。`spec.name` 是加过前缀的那个。
    ///
    /// # 失败一律回 `ToolResult::err`，不往上抛
    ///
    /// 工具失败是**模型要读的信息**，不是这一轮的错误。抛上去会中断整轮，
    /// 而模型本来可以看到「那台 server 断了」然后换个办法 —— 见
    /// `tool_error_becomes_a_visible_result_not_a_protocol_error`。
    pub async fn call(&self, spec: &ToolSpec, arguments: &serde_json::Value) -> ToolResult {
        let Some((server_name, tool)) = split_prefixed(&spec.name) else {
            return ToolResult::err(format!(
                "工具名 {} 不是 mcp__server__tool 的形状 —— 它不该被派到 MCP 这一侧",
                spec.name
            ));
        };
        let inner = self.inner.read().await;
        let Some(server) = inner.servers.get(server_name) else {
            return ToolResult::err(format!(
                "MCP server {server_name} 现在没有连接。它可能在启动时就连不上，\
                 也可能中途断了 —— 请用户检查 MCP 配置，或者换一个工具。"
            ));
        };

        // 对端要的是 object，而模型偶尔会给 null 或一个裸值。
        // 转不动就照实说，别把一个畸形请求发出去等它超时
        let args = match arguments {
            serde_json::Value::Object(m) => Some(m.clone()),
            serde_json::Value::Null => None,
            other => {
                return ToolResult::err(format!("参数必须是一个对象，收到的是 {other}"));
            }
        };

        // `CallToolRequestParams` 是 `#[non_exhaustive]`，只能走构造函数 ——
        // 这是好事：rmcp 后面加字段（SEP-1319 那套 task 元数据已经在里面了）
        // 不会把我们编译崩，也不会让我们默默传一个错的默认值
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(a) = args {
            params = params.with_arguments(a);
        }

        match server.service.call_tool(params).await {
            Ok(res) => {
                let text = flatten(&res);
                // 对端自己说这次失败了 —— 照它说的报，别把失败当成功
                if res.is_error.unwrap_or(false) {
                    ToolResult::err(text)
                } else {
                    ToolResult::ok(text)
                }
            }
            Err(e) => ToolResult::err(format!("调用 {} 失败：{e}", spec.name)),
        }
    }
}

/// 造一条启动子进程的命令。
///
/// # Windows 上必须绕 `cmd /C`，否则一条配置都跑不起来
///
/// Rust 的 `Command` 在 Windows 上只按 `.exe` 找程序 —— 它**不看 PATHEXT**。
/// 而 npm 装出来的 `npx` / `uvx` 是 `npx.cmd`，于是 `Command::new("npx")`
/// 直接返回 `program not found`。
///
/// 这不是一条边角情况：**Claude Code 的 MCP 配置清一色是 `npx -y ...`**，
/// 而我们唯一发布的桌面平台就是 Windows。不绕这一下，这个功能在真实用户
/// 那儿是零可用 —— 而症状只是一行「MCP server 连不上」，看起来像那个包
/// 没装好。
///
/// 代价说清楚：`cmd /C` 会**重新解析**参数，所以带 `&`、`^`、`|` 这类字符的
/// 参数可能被 cmd 抢走。判据因此收窄成「命令名没有扩展名」—— 直接写
/// `foo.exe` 的人拿到的仍然是原来那条不经 cmd 的路径。
#[cfg(windows)]
fn spawn_command(command: &str, args: &[String]) -> Command {
    let has_ext = std::path::Path::new(command).extension().is_some();
    if has_ext {
        let mut c = Command::new(command);
        c.args(args);
        return c;
    }
    let mut c = Command::new("cmd");
    c.arg("/C").arg(command).args(args);
    c
}

#[cfg(not(windows))]
fn spawn_command(command: &str, args: &[String]) -> Command {
    let mut c = Command::new(command);
    c.args(args);
    c
}

/// 把 `mcp__server__tool` 拆回两半。
///
/// **从左边找第二个 `__`**，不是从右边：server 名由用户起，可以含 `__`；
/// 工具名由对端给，同样可以。从右边找的话，一台叫 `a` 的 server 提供的
/// `b__c` 会被拆成 server=`a__b`、tool=`c` —— 于是查不到那台 server，
/// 报的是「没有连接」，而它连得好好的。
fn split_prefixed(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let at = rest.find("__")?;
    Some((&rest[..at], &rest[at + 2..]))
}

/// 把对端回的内容压成一段文本。
///
/// 只取文本部分：图片与资源引用现在没有进上下文的通道，硬塞一段 base64
/// 进去是纯烧 token。**跳过时留一行说明**，别让模型以为对端什么都没回。
fn flatten(res: &rmcp::model::CallToolResult) -> String {
    let mut parts = Vec::new();
    let mut skipped = 0usize;
    for c in &res.content {
        match &c.raw {
            RawContent::Text(t) => parts.push(t.text.clone()),
            _ => skipped += 1,
        }
    }
    if skipped > 0 {
        parts.push(format!(
            "（另有 {skipped} 段非文本内容，这个 agent 暂时接不了）"
        ));
    }
    if parts.is_empty() {
        "（对端没有返回任何内容）".to_string()
    } else {
        parts.join("\n")
    }
}

async fn connect_one(name: &str, sc: &ServerConfig) -> Result<Server, String> {
    let service = match &sc.transport {
        Transport::Stdio { command, args, env } => {
            let mut cmd = spawn_command(command, args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let transport =
                TokioChildProcess::new(cmd).map_err(|e| format!("拉不起子进程 {command}：{e}"))?;
            ().serve(transport)
                .await
                .map_err(|e| format!("握手失败：{e}"))?
        }
        Transport::Http { url, headers } => {
            if !headers.is_empty() {
                // 说出来而不是假装支持：一个带 Authorization 的 server 在
                // 这里会以 401 失败，而用户明明配了那个头
                tracing::warn!(
                    server = name,
                    "配置里的自定义请求头暂时没有接上，这台 server 会以不带头的方式连接"
                );
            }
            let transport = StreamableHttpClientTransport::from_uri(url.clone());
            ().serve(transport)
                .await
                .map_err(|e| format!("连不上 {url}：{e}"))?
        }
    };

    let listed = service
        .list_all_tools()
        .await
        .map_err(|e| format!("列工具失败：{e}"))?;

    let server: Arc<str> = Arc::from(name);
    let risk = sc.trust.risk();
    let specs = listed
        .into_iter()
        .map(|t| {
            let mut spec = ToolSpec::external(
                &server,
                &t.name,
                t.description.as_deref().unwrap_or("（对端没有给描述）"),
                serde_json::Value::Object((*t.input_schema).clone()),
            );
            // ★ 唯一一处可以降档的地方，而它读的是**用户配置**，
            //   不是对端自报的 annotations。见 config::Trust
            if sc.trust != Trust::Ask {
                spec.risk = risk;
            }
            spec
        })
        .collect();

    Ok(Server { service, specs })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从**左边**数第二个 `__` 拆，而不是从右边。
    ///
    /// 从右边拆的症状极具误导性：一台连得好好的 server，它那个名字里带
    /// `__` 的工具会报「MCP server xxx 现在没有连接」。
    #[test]
    fn the_prefix_splits_at_the_first_separator_not_the_last() {
        assert_eq!(split_prefixed("mcp__fs__read"), Some(("fs", "read")));
        assert_eq!(
            split_prefixed("mcp__fs__read__all"),
            Some(("fs", "read__all")),
            "工具名里的 __ 属于工具名，不能把它算进 server 名"
        );
        assert_eq!(split_prefixed("read_file"), None, "内置工具不该走到这儿");
        assert_eq!(split_prefixed("mcp__nofield"), None);
    }

    /// 空 hub 什么都没有，而且**不报错**。
    ///
    /// 绝大多数人不配 MCP，那条路必须是安静的。
    #[tokio::test]
    async fn an_empty_hub_offers_nothing_and_complains_about_nothing() {
        let h = McpHub::empty();
        assert!(h.specs().await.is_empty());
        assert!(h.status().await.is_empty());
    }

    /// 连不上的 server 出现在状态里，且带着原因。
    ///
    /// 这条钉的是「降级必须可见」：少了几个工具在用户那儿的表现是
    /// 「模型今天有点笨」，而那是最难报的一类故障。
    #[tokio::test]
    async fn a_server_that_will_not_start_is_reported_not_swallowed() {
        let cfg = McpConfig::parse(
            r#"{"mcpServers":{"ghost":{"command":"definitely-not-a-real-binary-xyz"}}}"#,
        )
        .unwrap();
        let hub = McpHub::connect(&cfg).await;

        assert!(hub.specs().await.is_empty(), "连不上就不该有工具");
        let st = hub.status().await;
        assert_eq!(st.len(), 1, "连不上的 server 必须出现在状态里");
        assert_eq!(st[0].name, "ghost");
        assert!(!st[0].connected);
        assert!(
            st[0].error.is_some(),
            "要带上原因，否则用户只知道『少了点什么』"
        );
    }

    /// `reload` 换掉的是**整份**，包括把上一次的失败清掉。
    ///
    /// 钉这条是因为失败是单独一张表：只换 servers 不换 failures 的话，
    /// 用户删掉那台连不上的 server、点重连，界面上它**还在报错** ——
    /// 而配置里已经没有它了。
    #[tokio::test]
    async fn reload_replaces_everything_including_the_old_failures() {
        let broken = McpConfig::parse(
            r#"{"mcpServers":{"ghost":{"command":"definitely-not-a-real-binary-xyz"}}}"#,
        )
        .unwrap();
        let hub = McpHub::connect(&broken).await;
        assert_eq!(hub.status().await.len(), 1);

        let after = hub.reload(&McpConfig::default()).await;
        assert!(
            after.is_empty(),
            "配置里已经没有它了，界面就不该继续报它的错"
        );
        assert!(
            hub.status().await.is_empty(),
            "reload 的返回值与后续查询必须一致"
        );
    }

    /// 派给一台不存在的 server 时，回一条模型读得懂的失败。
    #[tokio::test]
    async fn calling_a_disconnected_server_explains_itself() {
        let hub = McpHub::empty();
        let s: Arc<str> = Arc::from("gone");
        let spec = ToolSpec::external(&s, "do", "", serde_json::json!({}));
        let r = hub.call(&spec, &serde_json::json!({})).await;
        assert!(!r.ok);
        assert!(
            r.content.contains("没有连接"),
            "要说清是连接的问题，别让模型以为是参数写错了：{}",
            r.content
        );
    }
}
