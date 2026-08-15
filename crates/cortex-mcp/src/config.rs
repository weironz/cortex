//! MCP server 配置。
//!
//! # 形状照抄 Claude Code 的 `mcp.json`
//!
//! 不是懒。用户手上**已经有**这些文件了 —— 接第三方工具这件事的价值，
//! 一大半来自「他为别的 agent 配过的东西，在这儿直接能用」。发明一份
//! 自己的 schema 等于要求每个人为我们再配一遍，而配置项本身没有任何
//! 值得我们做得不一样的地方。
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "filesystem": {
//!       "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
//!     },
//!     "docs": { "type": "http", "url": "https://example.com/mcp" }
//!   }
//! }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use cortex_agent::Risk;
use cortex_core::{CortexError, Result};
use serde::Deserialize;

/// 一整份配置文件。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpConfig {
    /// key 就是 server 名，会成为工具名前缀的一部分（`mcp__{key}__{tool}`）。
    ///
    /// `BTreeMap` 而不是 `HashMap`：连接顺序、日志顺序、以及模型看到的工具
    /// 顺序都因此稳定。工具目录进的是 prompt，而一份每次重启都换顺序的
    /// prompt 会让**前缀缓存逐次失效** —— 那正是 `injection` 那套纪律
    /// 拼命在防的东西，不该从这儿漏回来。
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, ServerConfig>,
}

/// 一台 server 怎么连、以及它的工具算多高风险。
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(flatten)]
    pub transport: Transport,

    /// 把这台 server 的工具降到哪一档。**默认不降**。
    ///
    /// # 为什么降档是配置项，而不是读对端的 annotations
    ///
    /// MCP 的 `annotations.readOnlyHint` 是**服务端自报**的，而
    /// `ApprovalPolicy::decide` 只在 `risk < confirm_at` 时放行 ——
    /// 认它就等于让被闸的人自己决定要不要过闸。
    ///
    /// 降档必须是**用户对这台 server 的信任声明**：他知道自己装的是什么、
    /// 愿意为它免掉逐次确认。写在他自己的配置文件里，这件事才是他做的。
    ///
    /// 字段名刻意叫 `trust` 而不是 `risk`：配置里要表达的是「我多信它」，
    /// 不是「它有多危险」—— 后者我们判断不了。
    #[serde(default)]
    pub trust: Trust,

    /// 关掉但不删掉。调试时比注释掉一整段 JSON 方便，而 JSON 没有注释。
    #[serde(default)]
    pub disabled: bool,
}

/// 用户对一台 server 的信任程度，决定它的工具落在哪一档 [`Risk`]。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// 默认：每次调用都问。
    #[default]
    Ask,
    /// 当成写操作 —— 在默认策略（`confirm_at = Write`）下仍然要问，
    /// 但用户把 `confirm_at` 调到 `Execute` 时就不问了。
    ///
    /// 存在的意义是让「只读的文档服务器」与「能改我文件的服务器」在配置里
    /// 分得开，而不是只有「问」和「完全不问」两档。
    Write,
    /// 完全信任，按只读工具对待，默认策略下直接放行。
    ///
    /// ⚠️ 这是用户在说「这台 server 干什么我都认」。它拿到的信任
    /// **不低于内置的 `read_file`**。
    Trusted,
}

impl Trust {
    /// 这一档对应的 [`Risk`]。
    #[must_use]
    pub const fn risk(self) -> Risk {
        match self {
            Self::Ask => Risk::Execute,
            Self::Write => Risk::Write,
            Self::Trusted => Risk::Safe,
        }
    }
}

/// 怎么连上去。
///
/// # 为什么是 `untagged` 而不是 `tag = "type"`
///
/// Claude Code 的配置里，stdio 那些条目**根本不写 `type`** —— 就是一条
/// `command` 加一串 `args`。而 serde 的内部标签是必填的：写成
/// `tag = "type"` 之后，那类条目会以「missing field `type`」被拒，
/// 而那正是绝大多数条目的形状。
///
/// `untagged` 按顺序试，两个变体的必填字段不相交（`command` vs `url`），
/// 所以判别是确定的。代价是错误信息会退化成「data did not match any
/// variant」—— 用一条测试把两种形状都钉住，换那份兼容性。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Transport {
    /// 拉起一个子进程，走它的 stdin/stdout。
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// 额外环境变量。**不会**继承一份精简过的环境 —— 子进程拿到的是
        /// 本进程的环境加上这些，与 Claude Code 一致。
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Streamable HTTP。
    Http {
        url: String,
        /// 附加请求头，通常是 `Authorization`。
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// 桌面端读的用户级配置文件名。
///
/// 与 Claude Code 的用户作用域同义：**这台机器上我接哪些 server**。
pub const USER_FILE: &str = "mcp.json";

/// 容器里读的项目级配置文件名 —— 工作区根目录下的 `.mcp.json`。
///
/// 名字跟 Claude Code 的项目作用域**一模一样**，这是有意的：那个文件
/// 本来就躺在很多人的仓库根上、且已经被提交进版本库了。同名意味着
/// 「把项目搬进云沙箱」这件事**不需要用户再配一遍**。
pub const PROJECT_FILE: &str = ".mcp.json";

/// 这个进程的 MCP 配置该从哪儿读。
///
/// # 桌面端读用户目录，容器里读工作区根
///
/// 直觉上「容器里也读用户目录」是能工作的 —— 沙箱镜像把
/// `XDG_DATA_HOME` 指进了 `/workspace/.cortex/state`，那个文件确实能
/// 持久下来。**但没人写得进去**：那条路径长这样
///
/// ```text
/// /workspace/.cortex/state/cortex/users/usr_01JQZ.../mcp.json
/// ```
///
/// 里头嵌着一个用户事先不知道的 ULID，而且那是**进程自己管的状态目录**
/// （outbox、工作区绑定都在里面），不是给人手写配置的地方。
///
/// 工作区根则相反：路径就是项目根，能提交进版本库，跟着项目走。
/// 同一个人的两个项目想接不同的 server 是常态，而容器与卷本来就按项目分。
///
/// # 判据是 `ExecEnvironment`，不是「哪个文件存在」
///
/// 「存在就读」看着更宽容，实际是把两种部署形态的差别藏进一次文件系统
/// 探测：桌面端哪天在工作区里出现一个 `.mcp.json`（clone 下来的仓库自带
/// 一个，很常见），它就会**悄悄顶掉**用户自己那份。
///
/// 同一条教训在客户端的 `resolveBase` 上吃过一次：两处各判各的，就会
/// 出现「以为配上了但读的是另一个文件」，而那不报错。
///
/// # 桌面端**也不合并**读项目那份 —— 这条比上面那条更硬
///
/// Claude Code 是两个作用域都读的，但它在用项目那份之前**会问一次**。
/// 我们还没有那个询问界面，于是合并等于：clone 一个陌生仓库、绑上工作区，
/// 它自带的 `.mcp.json` 就在用户的真机上拉起了子进程 —— 没人点过同意。
///
/// 容器里没有这个问题：那本来就是隔离出来跑不受信代码的地方。
/// 等有了询问界面（见 roadmap 的 MCP 设置页），桌面端再合并也不迟。
///
/// # 工作区是用户可写的 —— 这不扩大攻击面
///
/// 也就是说 agent 能改自己下一轮加载哪些 server。但沙箱里本来就有
/// `shell`，`npx` 想跑现在就能跑 —— 配置文件只是把「谁决定」从即兴改成
/// 显式。而且外来工具一律 [`Risk::Execute`]（见
/// `cortex_agent::ToolSpec::external`），本来就每次都要过闸门。
///
/// # 容器里没有工作区时回落到用户目录
///
/// 那是装配错误（agentd 一定会传 `CORTEX_DEFAULT_WORKSPACE=/workspace`），
/// 回落的结果是读一个不存在的文件 = 没有 MCP。**这正是想要的降级**：
/// 一个装配不全的容器不该因为读不到配置而起不来。
#[must_use]
pub fn config_path(
    env: cortex_agent::ExecEnvironment,
    user_dir: &Path,
    workspace: Option<&Path>,
) -> std::path::PathBuf {
    match (env, workspace) {
        (cortex_agent::ExecEnvironment::Container, Some(ws)) => ws.join(PROJECT_FILE),
        _ => user_dir.join(USER_FILE),
    }
}

impl McpConfig {
    /// 从一个文件读。文件不存在 = 空配置，**不是错误**。
    ///
    /// 绝大多数人不配 MCP，而「没有配置文件」和「配置文件写错了」必须是
    /// 两种不同的结果：前者安静地什么都不做，后者要响。
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(CortexError::Invalid(format!(
                    "读不了 MCP 配置 {}：{e}",
                    path.display()
                )));
            }
        };
        Self::parse(&text)
            .map_err(|e| CortexError::Invalid(format!("MCP 配置 {} 解析失败：{e}", path.display())))
    }

    /// 解析一段 JSON。提出来只为**能测**——不落盘。
    pub fn parse(text: &str) -> std::result::Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// 实际要连的那些（去掉 `disabled`）。
    pub fn enabled(&self) -> impl Iterator<Item = (&String, &ServerConfig)> {
        self.servers.iter().filter(|(_, c)| !c.disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code 那份配置能原样读进来。
    ///
    /// 这条钉的是「用户不用为我们再配一遍」——接第三方工具的价值一大半在
    /// 这里。字段名或默认值哪天漂了，症状是用户把文件拷过来发现连不上，
    /// 而错误信息只会说某个字段缺失。
    #[test]
    fn a_claude_code_config_parses_as_is() {
        let c = McpConfig::parse(
            r#"{
              "mcpServers": {
                "filesystem": {
                  "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
                },
                "docs": { "type": "http", "url": "https://example.com/mcp" }
              }
            }"#,
        )
        .expect("Claude Code 的配置必须能原样解析");

        assert_eq!(c.servers.len(), 2);
        let fs = &c.servers["filesystem"];
        assert!(
            matches!(&fs.transport, Transport::Stdio { command, args, .. }
                if command == "npx" && args.len() == 3),
            "没有 type 字段时必须当成 stdio —— Claude Code 那边绝大多数条目都不写它"
        );
        assert!(matches!(
            &c.servers["docs"].transport,
            Transport::Http { .. }
        ));
    }

    /// 没配 trust 就是每次都问，而且那一档必须是最高风险。
    #[test]
    fn the_default_trust_level_asks_every_time() {
        let c = McpConfig::parse(r#"{"mcpServers":{"x":{"command":"y"}}}"#).unwrap();
        assert_eq!(c.servers["x"].trust, Trust::Ask);
        assert_eq!(
            Trust::Ask.risk(),
            Risk::Execute,
            "默认档必须落在最高风险上 —— 降档只能是用户显式写下的信任声明"
        );
    }

    /// 三档信任映射到三档风险，且**顺序不能反**。
    ///
    /// 反了不会有任何编译错误，症状是用户写 `"trust": "trusted"` 之后
    /// 反而每次都被问，或者更糟：写 `"ask"` 却一次都不问。
    #[test]
    fn trust_maps_monotonically_onto_risk() {
        assert!(Trust::Trusted.risk() < Trust::Write.risk());
        assert!(Trust::Write.risk() < Trust::Ask.risk());
    }

    /// 文件不存在 = 空配置，不是错误。
    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let c = McpConfig::load(&dir.path().join("nope.json")).expect("没有配置文件是正常情况");
        assert!(c.servers.is_empty());
    }

    /// 但写错了要响。
    #[test]
    fn a_broken_config_file_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp.json");
        std::fs::write(&p, "{ this is not json").unwrap();
        let e = McpConfig::load(&p).expect_err("坏配置必须报错，不能静默当成没配");
        assert!(
            e.to_string().contains("解析失败"),
            "错误信息要说清是解析问题：{e}"
        );
    }

    /// 容器读工作区，桌面端读用户目录 —— **两个方向都要断言**。
    ///
    /// 只测容器那一支的话，把整个函数写成「永远返回工作区」也能过，
    /// 而那会让桌面端用户自己那份 `mcp.json` 静默失效。
    #[test]
    fn a_container_reads_the_workspace_and_a_desktop_reads_the_user_dir() {
        use cortex_agent::ExecEnvironment::{Container, LocalMachine, None as NoEnv};
        let user = Path::new("/home/u/.cortex/01J");
        let ws = Path::new("/workspace");

        assert_eq!(
            config_path(Container, user, Some(ws)),
            ws.join(".mcp.json"),
            "云端会话的配置必须落在项目根上 —— 用户目录那条路径里嵌着 \
             他不知道的 ULID，写不进去"
        );
        for env in [LocalMachine, NoEnv] {
            assert_eq!(
                config_path(env, user, Some(ws)),
                user.join("mcp.json"),
                "桌面端即便绑了工作区也读用户目录 —— 否则 clone 下来的仓库 \
                 自带一个 .mcp.json 就会悄悄顶掉用户自己那份"
            );
        }
    }

    /// 容器里没有工作区 = 配置错误，回落成「没有 MCP」而不是起不来。
    #[test]
    fn a_container_without_a_workspace_degrades_instead_of_failing() {
        use cortex_agent::ExecEnvironment::Container;
        let user = Path::new("/tmp/ephemeral");
        assert_eq!(config_path(Container, user, None), user.join(USER_FILE));
        // 那个路径在容器里不存在，而不存在 = 空配置（见 load 的文档）
        assert!(
            McpConfig::load(&config_path(Container, user, None))
                .expect("读不到就是没配，不该是错误")
                .servers
                .is_empty()
        );
    }

    #[test]
    fn disabled_servers_are_skipped_but_kept() {
        let c = McpConfig::parse(
            r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y","disabled":true}}}"#,
        )
        .unwrap();
        assert_eq!(c.servers.len(), 2, "关掉的仍然在配置里");
        let live: Vec<&str> = c.enabled().map(|(k, _)| k.as_str()).collect();
        assert_eq!(live, vec!["a"]);
    }
}
