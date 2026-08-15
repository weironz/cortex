//! 官方 MCP Registry 的客户端。
//!
//! <https://registry.modelcontextprotocol.io>
//!
//! # 为什么是 agent 去取，而不是客户端直连
//!
//! 因为**转换在这边**：注册表那份 schema 与我们的 [`ServerConfig`] 不是
//! 一回事，中间那层判断（哪种包用哪个运行时、参数怎么排）有边界情况、
//! 要单测。取数跟着转换走，Dart 那边就只剩渲染 —— 否则桌面端一份转换、
//! 将来云端界面再一份，两份迟早不一致。
//!
//! # 两个只有真去打了才知道的坑
//!
//! 1. **`version=latest` 必须带。** 不带的话每个 server 的**每个历史版本**
//!    都是独立一条，搜「filesystem」出来的前十条可能是同一个包的十个版本。
//! 2. **`runtimeHint` 多数是 null。** 真实数据里绝大多数条目没有它 ——
//!    所以运行时得从 `registryType`（npm/pypi/oci/nuget）推，把 hint 当
//!    必填字段用的话，能装的条目会少一个数量级。

use std::collections::BTreeMap;

use cortex_core::{CortexError, Result};
use serde::{Deserialize, Serialize};

use crate::config::{ServerConfig, Transport, Trust};

const BASE: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";

/// 一条能装的东西。
///
/// **一个条目可能有好几种装法**（npm 一种、远程一种），所以 `installs`
/// 是个列表而不是一个 —— 只留第一种会让「这台其实有远程版」被静默丢掉。
#[derive(Debug, Clone, Serialize)]
pub struct RegistryEntry {
    /// 注册表里的全名，形如 `io.github.foo/bar`。
    pub name: String,
    /// 人读的标题。多数条目没有，界面回落到 `name`。
    pub title: Option<String>,
    pub description: String,
    pub version: String,
    pub repository: Option<String>,
    /// 建议的 server 名（`mcp__{它}__tool`）。用户可以改。
    pub suggested_name: String,
    /// 各种装法。**空的意味着我们装不了它**（只有我们不认的包类型），
    /// 界面该把它标成不可安装，而不是给一个点了没反应的按钮。
    pub installs: Vec<Install>,
}

/// 一种装法：直接就是可落盘的那份配置，外加要用户填的环境变量。
#[derive(Debug, Clone, Serialize)]
pub struct Install {
    /// `npm` / `pypi` / `oci` / `nuget` / `remote`。给界面显示用。
    pub kind: String,
    pub server: ServerConfig,
    /// 这台 server 认的环境变量。**值不在这里** —— 要用户自己填。
    pub env: Vec<EnvVarSpec>,
}

/// 一个环境变量的说明。
#[derive(Debug, Clone, Serialize)]
pub struct EnvVarSpec {
    pub name: String,
    pub description: Option<String>,
    /// 不填就跑不起来。界面该拦住「必填还空着就点安装」。
    pub required: bool,
    /// 是密钥。界面**不回显**它的值，落盘之后也不读回来给界面。
    pub secret: bool,
    pub default: Option<String>,
}

/// 搜一下。`query` 为空就是「随便看看」（注册表按名字排）。
///
/// # Errors
/// 网络失败、非 2xx、或者响应不是预期的 JSON。
pub async fn search(
    client: &reqwest::Client,
    query: &str,
    limit: u32,
) -> Result<Vec<RegistryEntry>> {
    // `version=latest` 见模块文档 —— 漏了它列表会被历史版本淹掉
    let mut req = client
        .get(BASE)
        .query(&[("version", "latest")])
        .query(&[("limit", limit.to_string())]);
    if !query.trim().is_empty() {
        req = req.query(&[("search", query.trim())]);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| CortexError::Invalid(format!("连不上 MCP 注册表：{e}")))?;
    if !resp.status().is_success() {
        return Err(CortexError::Invalid(format!(
            "MCP 注册表回了 {}",
            resp.status()
        )));
    }
    let body: ListResponse = resp
        .json()
        .await
        .map_err(|e| CortexError::Invalid(format!("MCP 注册表的响应读不懂：{e}")))?;

    Ok(body
        .servers
        .into_iter()
        .map(|w| convert(w.server))
        .collect())
}

// ── 注册表那侧的 schema ────────────────────────────────
//
// 只声明我们用得上的字段。**不加 `deny_unknown_fields`**：注册表在演进，
// 多一个字段就整份解析失败的话，功能会在某天早上无声地坏掉。

#[derive(Deserialize)]
struct ListResponse {
    #[serde(default)]
    servers: Vec<Wrapper>,
}

#[derive(Deserialize)]
struct Wrapper {
    server: RawServer,
}

#[derive(Deserialize)]
struct RawServer {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    repository: Option<Repository>,
    #[serde(default)]
    packages: Vec<Package>,
    #[serde(default)]
    remotes: Vec<Remote>,
}

#[derive(Deserialize)]
struct Repository {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Package {
    #[serde(default)]
    registry_type: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    version: Option<String>,
    /// 官方文档说它是「提示」，实测**多数条目没有**。见模块文档。
    #[serde(default)]
    runtime_hint: Option<String>,
    #[serde(default)]
    runtime_arguments: Vec<Argument>,
    #[serde(default)]
    package_arguments: Vec<Argument>,
    #[serde(default)]
    environment_variables: Vec<RawEnv>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Argument {
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEnv {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    is_required: bool,
    #[serde(default)]
    is_secret: bool,
    #[serde(default)]
    default: Option<String>,
}

#[derive(Deserialize)]
struct Remote {
    url: String,
}

// ── 转换 ────────────────────────────────────────────

fn convert(s: RawServer) -> RegistryEntry {
    let suggested_name = suggested_name_of(&s.name);
    let mut installs: Vec<Install> = s.packages.iter().filter_map(install_from_package).collect();
    installs.extend(s.remotes.iter().map(|r| Install {
        kind: "remote".to_owned(),
        server: ServerConfig {
            transport: Transport::Http {
                url: r.url.clone(),
                headers: BTreeMap::new(),
            },
            trust: Trust::default(),
            disabled: false,
        },
        env: Vec::new(),
    }));

    RegistryEntry {
        name: s.name,
        title: s.title,
        description: s.description,
        version: s.version,
        repository: s.repository.and_then(|r| r.url),
        suggested_name,
        installs,
    }
}

/// `io.github.foo/bar-mcp` → `bar`。
fn suggested_name_of(full: &str) -> String {
    let last = full.rsplit('/').next().unwrap_or(full);
    let trimmed = last
        .trim_start_matches("mcp-server-")
        .trim_start_matches("mcp-")
        .trim_end_matches("-mcp-server")
        .trim_end_matches("-mcp");
    crate::paste::sanitize(trimmed)
}

/// 一个包 → 一条命令。认不出包类型就**回 None**，界面据此标成装不了。
///
/// 回 None 而不是硬凑一条命令：凑出来的那条跑不起来，而失败会发生在
/// 用户点完安装、等了十几秒之后，报的还是一个和「不支持这种包」毫无
/// 关系的错。
fn install_from_package(p: &Package) -> Option<Install> {
    let id = if p.identifier.is_empty() {
        return None;
    } else {
        &p.identifier
    };
    // `pkg@version`：钉版本，否则用户装的和界面显示的可能不是同一个东西
    let pinned = match &p.version {
        Some(v) if !v.is_empty() => format!("{id}@{v}"),
        _ => id.clone(),
    };

    // 运行时优先信 hint，没有再按包类型推 —— 实测多数条目没有 hint
    let runtime = p
        .runtime_hint
        .as_deref()
        .unwrap_or(match p.registry_type.as_str() {
            "npm" => "npx",
            "pypi" => "uvx",
            "oci" => "docker",
            "nuget" => "dnx",
            _ => return None,
        });

    let extra: Vec<String> = p
        .runtime_arguments
        .iter()
        .chain(p.package_arguments.iter())
        .filter_map(|a| a.value.clone().or_else(|| a.name.clone()))
        .collect();

    // 每种运行时的「非交互」姿势不同，而漏掉它的症状是子进程停在一个
    // 没人看得见的确认提示上，表现为「连接超时」
    let (command, mut args) = match runtime {
        "npx" => ("npx", vec!["-y".to_owned(), pinned]),
        "uvx" => ("uvx", vec![pinned]),
        "dnx" => ("dnx", vec![pinned, "--yes".to_owned()]),
        "docker" => (
            "docker",
            vec![
                "run".to_owned(),
                "-i".to_owned(),
                "--rm".to_owned(),
                // OCI 用的是 `image:tag`，不是 `pkg@ver`
                match &p.version {
                    Some(v) if !v.is_empty() => format!("{id}:{v}"),
                    _ => id.clone(),
                },
            ],
        ),
        _ => return None,
    };
    args.extend(extra);

    Some(Install {
        kind: if p.registry_type.is_empty() {
            runtime.to_owned()
        } else {
            p.registry_type.clone()
        },
        server: ServerConfig {
            transport: Transport::Stdio {
                command: command.to_owned(),
                args,
                // 值由用户在界面上填，这里只声明形状
                env: BTreeMap::new(),
            },
            trust: Trust::default(),
            disabled: false,
        },
        env: p
            .environment_variables
            .iter()
            .map(|e| EnvVarSpec {
                name: e.name.clone(),
                description: e.description.clone(),
                required: e.is_required,
                secret: e.is_secret,
                default: e.default.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture 是**真的抓下来的**（2026-08-15，`?search=filesystem&version=latest`）。
    ///
    /// 手编的 fixture 只能证明代码和我对 schema 的想象一致。这一份能证明
    /// 它和注册表真实吐出来的东西一致 —— 那两件事此前已经差过一次
    /// （`runtimeHint` 我原以为是必填的）。
    const REAL_NPM: &str = r#"{"servers":[{"server":{
      "name":"com.pulsemcp/remote-filesystem",
      "description":"MCP server for remote filesystem operations on cloud storage (Google Cloud Storage).",
      "repository":{"url":"https://github.com/pulsemcp/mcp-servers","source":"github"},
      "version":"0.1.5",
      "packages":[{
        "registryType":"npm",
        "registryBaseUrl":"https://registry.npmjs.org",
        "identifier":"remote-filesystem-mcp-server",
        "version":"0.1.5",
        "runtimeHint":"npx",
        "transport":{"type":"stdio"},
        "runtimeArguments":[{"value":"-y","type":"positional"}],
        "environmentVariables":[
          {"description":"Google Cloud Storage bucket name.","isRequired":true,"name":"GCS_BUCKET"},
          {"description":"Service account private key.","isSecret":true,"name":"GCS_PRIVATE_KEY"}
        ]
      }]
    },"_meta":{}}]}"#;

    fn parse(json: &str) -> Vec<RegistryEntry> {
        let body: ListResponse = serde_json::from_str(json).expect("fixture 应能解析");
        body.servers
            .into_iter()
            .map(|w| convert(w.server))
            .collect()
    }

    #[test]
    fn a_real_npm_entry_becomes_a_runnable_npx_command() {
        let e = parse(REAL_NPM).pop().expect("应有一条");
        assert_eq!(
            e.suggested_name, "remote-filesystem",
            "取斜杠后那段，去掉 mcp 缀"
        );

        let i = e.installs.first().expect("npm 包必须能装");
        assert_eq!(i.kind, "npm");
        match &i.server.transport {
            Transport::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &["-y", "remote-filesystem-mcp-server@0.1.5", "-y"],
                    "版本要钉住；runtimeArguments 里那个 -y 照样带上（宁可重复也不猜它想干什么）"
                );
            }
            other => panic!("npm 包应是 stdio，得到 {other:?}"),
        }

        assert_eq!(i.env.len(), 2);
        let secret = i
            .env
            .iter()
            .find(|v| v.secret)
            .expect("私钥那条要标成 secret");
        assert_eq!(secret.name, "GCS_PRIVATE_KEY");
        assert!(
            i.env.iter().any(|v| v.required && v.name == "GCS_BUCKET"),
            "必填要标出来，否则用户装完才发现起不来"
        );
    }

    /// **没有 `runtimeHint` 时按 `registryType` 推。**
    ///
    /// 这条是最要紧的一条：实测绝大多数条目都没有 hint。把它当必填字段用
    /// 的话，注册表里能装的条目会少一个数量级，而症状是「列表里大半都
    /// 装不了」——看起来像注册表的问题。
    #[test]
    fn a_missing_runtime_hint_falls_back_to_the_registry_type() {
        let entries = parse(
            r#"{"servers":[{"server":{
                 "name":"ai.example/thing","version":"1.0.0",
                 "packages":[{"registryType":"pypi","identifier":"thing-mcp","version":"1.0.0"}]
               }}]}"#,
        );
        let i = entries[0].installs.first().expect("pypi 也要能装");
        assert!(matches!(
            &i.server.transport,
            Transport::Stdio { command, args, .. }
                if command == "uvx" && args == &["thing-mcp@1.0.0"]
        ));
    }

    /// 远程条目直接就是 HTTP，不需要本机有 node/python。
    #[test]
    fn a_remote_entry_becomes_an_http_server() {
        let entries = parse(
            r#"{"servers":[{"server":{
                 "name":"ac.inference.sh/mcp","version":"1.0.1",
                 "remotes":[{"type":"streamable-http","url":"https://api.inference.sh/mcp"}]
               }}]}"#,
        );
        let i = entries[0].installs.first().expect("remote 必须能装");
        assert_eq!(i.kind, "remote");
        assert!(matches!(
            &i.server.transport,
            Transport::Http { url, .. } if url == "https://api.inference.sh/mcp"
        ));
    }

    /// 认不出的包类型 → `installs` 是空的，而不是一条跑不起来的命令。
    ///
    /// 硬凑的话，失败会发生在用户点完安装、等十几秒之后，报的还是一个
    /// 和「不支持这种包」毫无关系的错。
    #[test]
    fn an_unsupported_package_type_is_not_installable_rather_than_broken() {
        let entries = parse(
            r#"{"servers":[{"server":{
                 "name":"x/y","version":"1","packages":[{"registryType":"brand-new","identifier":"z"}]
               }}]}"#,
        );
        assert!(
            entries[0].installs.is_empty(),
            "不认的包类型该是「装不了」，不是「装了跑不起来」"
        );
    }

    /// 注册表加字段不该把整份响应打崩。
    #[test]
    fn unknown_fields_from_a_newer_registry_are_ignored() {
        let entries = parse(
            r#"{"servers":[{"server":{
                 "name":"x/y","version":"1","brandNewField":{"a":1},
                 "remotes":[{"type":"streamable-http","url":"https://x/mcp","futureThing":true}]
               }}],"metadata":{"nextCursor":"…"}}"#,
        );
        assert_eq!(entries.len(), 1, "多出来的字段该被忽略，不该整份解析失败");
    }
}
