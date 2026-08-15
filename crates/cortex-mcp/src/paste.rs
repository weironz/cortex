//! 「粘一段东西进来」→ 一份配置。
//!
//! # 为什么这是产品面而不是便利功能
//!
//! 真实世界里没人从零填表单：**人们从 README 里复制**。而 README 里那段
//! 长三种样子 —— 一段 `npx -y …`、一个 `https://…/mcp`、或者一整块
//! `{"mcpServers": …}`。三种都吃下来，「接一台 server」就是一次粘贴；
//! 只认表单的话，同一件事要人肉拆成五个字段。
//!
//! # 为什么在 Rust 里而不是 Dart 里
//!
//! 这是有边界情况的解析（引号、`--` 之后的东西、scope 包名），而边界情况
//! 需要单测。放在客户端意味着桌面端一份、将来云端界面再一份，两份的判形
//! 迟早不一致 —— 症状是同一段文本在两个地方粘出不同的配置。

use std::collections::BTreeMap;

use cortex_core::{CortexError, Result};

use crate::config::{McpConfig, ServerConfig, Transport, Trust, valid_server_name};

/// 把用户粘的一段文本解析成配置。**不落盘**。
///
/// 回的是一份 [`McpConfig`] 而不是单台：粘一整块 `mcpServers` JSON 时里面
/// 本来就可能有好几台，而「只取第一台」会**静默丢掉**其余的。
///
/// # Errors
/// 认不出形状、JSON 解析失败、或者名字不合法（见 [`valid_server_name`]）。
pub fn parse_pasted(text: &str) -> Result<McpConfig> {
    let t = text.trim();
    if t.is_empty() {
        return Err(CortexError::Invalid("粘进来的是空的".into()));
    }

    let cfg = if t.starts_with('{') {
        from_json(t)?
    } else if t.starts_with("http://") || t.starts_with("https://") {
        one(derive_name_from_url(t), http(t))
    } else {
        from_command(t)?
    };

    for name in cfg.servers.keys() {
        if !valid_server_name(name) {
            return Err(CortexError::Invalid(format!(
                "server 名 {name:?} 不能用：只允许字母、数字、`-`、`_`、`.`，且不能含 `__`"
            )));
        }
    }
    Ok(cfg)
}

fn one(name: String, transport: Transport) -> McpConfig {
    let mut servers = BTreeMap::new();
    servers.insert(
        name,
        ServerConfig {
            transport,
            trust: Trust::default(),
            disabled: false,
        },
    );
    McpConfig { servers }
}

fn http(url: &str) -> Transport {
    Transport::Http {
        url: url.to_owned(),
        headers: BTreeMap::new(),
    }
}

/// 一整块 JSON。两种形状都吃：带 `mcpServers` 外壳的，和单台裸的。
///
/// 单台裸的那种（`{"command": "npx", …}`）在 README 里很常见 —— 作者写的是
/// 「把这段塞进你的 mcpServers 里」。认它就少一次「你还得自己包一层」。
fn from_json(text: &str) -> Result<McpConfig> {
    let bad = |e: serde_json::Error| CortexError::Invalid(format!("JSON 解析失败：{e}"));

    // 先按整份配置试。`mcpServers` 有 `#[serde(default)]`，所以一个不相干的
    // JSON 也会「成功」解析成空配置 —— 空的就当没认出来，继续往下试
    if let Ok(cfg) = McpConfig::parse(text)
        && !cfg.servers.is_empty()
    {
        return Ok(cfg);
    }

    // 再按单台裸的试
    let sc: ServerConfig = serde_json::from_str(text).map_err(bad)?;
    let name = match &sc.transport {
        Transport::Stdio { command, args, .. } => derive_name_from_argv(command, args),
        Transport::Http { url, .. } => derive_name_from_url(url),
    };
    let mut servers = BTreeMap::new();
    servers.insert(name, sc);
    Ok(McpConfig { servers })
}

/// 一条命令行。
fn from_command(text: &str) -> Result<McpConfig> {
    let argv = split_argv(text);
    let Some((command, args)) = argv.split_first() else {
        return Err(CortexError::Invalid("认不出这段是什么".into()));
    };
    let name = derive_name_from_argv(command, args);
    Ok(one(
        name,
        Transport::Stdio {
            command: command.clone(),
            args: args.to_vec(),
            env: BTreeMap::new(),
        },
    ))
}

/// 按空白切，但**认引号**。
///
/// 路径带空格是 Windows 的日常（`C:\Program Files\…`），而 README 里那种
/// 路径就是用引号括起来的。裸切的话它会碎成两个参数，然后 server 起不来，
/// 报的是一个和引号毫无关系的错。
fn split_argv(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, _) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, _) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 从 URL 猜个名字：取主机名第一段，跳过 `www` / `api` 这类没信息的。
fn derive_name_from_url(url: &str) -> String {
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':', '?'])
        .next()
        .unwrap_or("");
    let mut labels = host.split('.').filter(|l| !l.is_empty());
    let first = labels.next().unwrap_or("");
    let name = if matches!(first, "www" | "api" | "mcp") {
        labels.next().unwrap_or(first)
    } else {
        first
    };
    sanitize(name)
}

/// 从命令行猜个名字：**取第一个不像选项的参数**，当它是包名。
///
/// `npx -y @modelcontextprotocol/server-filesystem /tmp` → `filesystem`。
/// 猜错了用户能改 —— 这一步的意义是让「粘贴 → 确认」两下能走完，
/// 而不是逼他先想一个名字。
fn derive_name_from_argv(command: &str, args: &[String]) -> String {
    let pkg = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or(command, String::as_str);
    // scope 前缀（`@org/`）与路径都不进名字
    let base = pkg.rsplit(['/', '\\']).next().unwrap_or(pkg);
    let base = base.split('@').next().unwrap_or(base); // 去掉 `pkg@1.2.3` 的版本
    let base = base
        .trim_end_matches(".js")
        .trim_end_matches(".py")
        .trim_end_matches(".exe");
    // 这些词每台 server 都有，留着等于每台都叫 mcp-server-x
    let base = base
        .trim_start_matches("mcp-server-")
        .trim_start_matches("server-")
        .trim_start_matches("mcp-")
        .trim_end_matches("-mcp-server")
        .trim_end_matches("-mcp");
    sanitize(base)
}

/// 收拢成一个合法 server 名。空的话给个兜底，别回空串。
pub(crate) fn sanitize(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_ascii_lowercase();
    if s.is_empty() { "server".to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(cfg: &McpConfig) -> (&String, &ServerConfig) {
        assert_eq!(cfg.servers.len(), 1, "这几条用例都只该出一台");
        cfg.servers.iter().next().expect("刚断言过非空")
    }

    /// README 里最常见的那一行。
    #[test]
    fn an_npx_command_line_becomes_a_stdio_server() {
        let cfg = parse_pasted("npx -y @modelcontextprotocol/server-filesystem C:/tmp")
            .expect("这是最典型的一行");
        let (name, sc) = only(&cfg);
        assert_eq!(name, "filesystem", "名字要从包名猜出来，别让用户先想一个");
        match &sc.transport {
            Transport::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &["-y", "@modelcontextprotocol/server-filesystem", "C:/tmp"]
                );
            }
            other => panic!("命令行必须解析成 stdio，得到 {other:?}"),
        }
    }

    /// 带空格的路径必须靠引号保住。
    ///
    /// 裸切的话 `C:\Program Files\x\y.exe` 会碎成两个参数，server 起不来，
    /// 而报错和引号毫无关系 —— Windows 上这是日常路径。
    #[test]
    fn a_quoted_path_with_spaces_stays_one_argument() {
        let cfg = parse_pasted(r#"node "C:/Program Files/tool/index.js" --port 3000"#)
            .expect("带引号的命令行要能解析");
        let (_, sc) = only(&cfg);
        match &sc.transport {
            Transport::Stdio { args, .. } => {
                assert_eq!(
                    args[0], "C:/Program Files/tool/index.js",
                    "引号里的空格不该切开"
                );
                assert_eq!(args.len(), 3);
            }
            other => panic!("应是 stdio，得到 {other:?}"),
        }
    }

    #[test]
    fn a_bare_url_becomes_an_http_server() {
        let cfg = parse_pasted("https://docs.example.com/mcp").expect("URL 要能认");
        let (name, sc) = only(&cfg);
        assert_eq!(name, "docs");
        assert!(
            matches!(&sc.transport, Transport::Http { url, .. } if url == "https://docs.example.com/mcp")
        );
    }

    /// 一整块 `mcpServers` JSON —— 而且**多台都要留下**。
    ///
    /// 「只取第一台」是最容易写出来的实现，症状是用户粘了三台、界面出现
    /// 一台，另外两台无声无息。
    #[test]
    fn a_whole_mcp_servers_block_keeps_every_entry() {
        let cfg = parse_pasted(
            r#"{"mcpServers":{
                 "a":{"command":"npx","args":["-y","a"]},
                 "b":{"type":"http","url":"https://b.example.com/mcp"}
               }}"#,
        )
        .expect("整块 JSON 要能认");
        assert_eq!(cfg.servers.len(), 2, "粘几台就该出几台");
        assert!(cfg.servers.contains_key("a") && cfg.servers.contains_key("b"));
    }

    /// 单台裸的 JSON —— README 里常写成「把这段塞进你的 mcpServers 里」。
    #[test]
    fn a_bare_single_server_object_also_works() {
        let cfg =
            parse_pasted(r#"{"command":"uvx","args":["mcp-server-git"]}"#).expect("裸的单台也要认");
        let (name, _) = only(&cfg);
        assert_eq!(
            name, "git",
            "`mcp-server-` 前缀不进名字，否则每台都叫 mcp-server-x"
        );
    }

    /// 名字含 `__` 的配置必须**当场拒**，不能落盘。
    ///
    /// 它不会报错，只会让那台 server 的每次工具调用都说「没有连接」——
    /// 因为 `split_prefixed` 从第一个 `__` 拆，查到的是半个名字。
    #[test]
    fn a_name_with_the_separator_in_it_is_refused_up_front() {
        let err = parse_pasted(r#"{"mcpServers":{"a__b":{"command":"x"}}}"#)
            .expect_err("含 __ 的名字必须拒");
        assert!(
            format!("{err}").contains("__"),
            "错误信息要指出是分隔符的问题，否则用户不知道改哪里：{err}"
        );
    }

    #[test]
    fn an_empty_paste_says_so() {
        assert!(parse_pasted("   \n ").is_err(), "空的要报错，不是静默成功");
    }
}
