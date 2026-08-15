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
