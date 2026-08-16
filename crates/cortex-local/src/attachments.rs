//! 附件 → 模型输入的分岔投放。
//!
//! # 为什么在这里做，而不是 agentd
//!
//! 本机会话与云端会话跑的是**同一个** `cortex-local`（差别只有
//! `--exec-env`），投放逻辑放在这儿就只有一份实现；放 agentd 的话
//! 本机会话（根本不经过它）要么没有附件、要么再写一份。字节经
//! [`crate::remote::Remote`] 取：桌面端打部署入口（用户 bearer），
//! 容器打回调（委托令牌，agentd 从对象存储中转 —— 沙箱够不到
//! 对象存储是有意的设计，见 egress allowlist）。
//!
//! # 三条岔路（按内容类型与大小，不按扩展名）
//!
//! - **图片** → 模型的多模态 Image 块。模型原生能读，转录一遍反而丢信息。
//! - **小文本** → 直接进上下文（带文件名的围栏块）。
//! - **其余 / 太大** → 落进工作区 `attachments/` 目录，让模型用文件工具去读。
//!
//! 判据用的是**服务端嗅探出的 MIME**（登记 blob 时从字节头算的，
//! 不信客户端声明），所以伪装成 `.txt` 的 PNG 走的是图片那条路。
//!
//! # 任何失败都不挡这一轮
//!
//! 取不回字节（离线、blob 被清、太大）就落成一条「附件不可用」的说明。
//! 用户只是想说句话 —— 附件坏了不该让整轮对话失败，这与
//! 「记忆未连接照样能聊」是同一个立场。

use std::path::{Path, PathBuf};

use cortex_llm::vision::MAX_IMAGE_BYTES;
use cortex_proto::dto::AttachmentRef;

use crate::remote::Remote;

/// 文本附件直接进上下文的上限。
///
/// 32 KiB ≈ 一万多 token：再大就不是「随手贴段配置」而是数据文件了，
/// 后者该走工作区让模型按需读 —— 整个塞进上下文既贵又挤掉历史轮次。
const INLINE_TEXT_MAX: u64 = 32 * 1024;

/// 落进工作区的单文件上限。与 agentd 侧快照/导出的 512 MiB 口径一致
/// （`MAX_EXPORT_BYTES`）—— 超过它的东西在这套系统里本来就搬不动。
const WORKSPACE_FILE_MAX: u64 = 512 * 1024 * 1024;

/// 工作区里放附件的子目录。
///
/// 不直接铺在根上：工作区是模型的操作台，一轮几个附件就会把它的
/// `ls` 淹掉；归到一个固定目录里，路径还能在消息里说清。
const ATTACHMENT_DIR: &str = "attachments";

/// 一个附件最终去了哪。
pub enum Placement {
    /// 进多模态块。字节在手，组消息时 `with_image_bytes`。
    Image {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    /// 小文本，直接进上下文。
    InlineText { name: String, text: String },
    /// 落进了工作区，`rel` 是相对工作区根的路径（正斜杠）。
    WorkspaceFile {
        name: String,
        mime: String,
        size: u64,
        rel: String,
    },
    /// 这一轮用不上它，`reason` 会原样进给模型看的说明。
    Unavailable { name: String, reason: String },
}

/// 把本轮的附件逐个投放到位。**不失败** —— 单个附件的问题落成
/// [`Placement::Unavailable`]，轮次照常跑。
pub async fn place(
    remote: &Remote,
    refs: &[AttachmentRef],
    workspace: Option<&Path>,
    model_can_see: bool,
) -> Vec<Placement> {
    let mut out = Vec::with_capacity(refs.len());
    // 已占用的目标文件名（本轮内防撞；跨轮的撞名靠哈希前缀）
    let mut taken: Vec<String> = Vec::new();
    for r in refs {
        out.push(place_one(remote, r, workspace, model_can_see, &mut taken).await);
    }
    out
}

async fn place_one(
    remote: &Remote,
    r: &AttachmentRef,
    workspace: Option<&Path>,
    model_can_see: bool,
    taken: &mut Vec<String>,
) -> Placement {
    let name = display_name(r);

    // 先只拿响应头（mime + 长度），再决定要不要把身子读进内存 ——
    // 一个 300 MB 的 zip 不该先进内存再发现「哦，该走工作区」。
    let resp = match remote.blob_response(&r.hash).await {
        Ok(resp) => resp,
        Err(e) => {
            return Placement::Unavailable {
                name,
                reason: format!("取不到内容（{e}）"),
            };
        }
    };
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| "application/octet-stream".to_string(), str::to_string);
    let size = resp.content_length();

    // ── 岔路 1：图片 ──
    if mime.starts_with("image/") && model_can_see {
        match read_capped(resp, MAX_IMAGE_BYTES as u64).await {
            Ok(bytes) => {
                return Placement::Image { name, mime, bytes };
            }
            // 超限不是错误，是换条路：太大的图当文件落工作区
            Err(ReadCapped::TooLarge) => {}
            Err(ReadCapped::Transport(e)) => {
                return Placement::Unavailable {
                    name,
                    reason: format!("下载中断（{e}）"),
                };
            }
        }
        // 掉出来 = 图太大 → 走工作区那条。字节已经消耗，得重新请求
        return place_in_workspace(remote, r, name, mime, size, workspace, taken).await;
    }

    // ── 岔路 2：小文本 ──
    if is_texty(&mime) && size.is_none_or(|s| s <= INLINE_TEXT_MAX) {
        match read_capped(resp, INLINE_TEXT_MAX).await {
            Ok(bytes) => {
                // 嗅探说是文本但字节不是合法 UTF-8：别硬塞乱码进上下文
                if let Ok(text) = String::from_utf8(bytes) {
                    return Placement::InlineText { name, text };
                }
            }
            Err(ReadCapped::TooLarge) => {}
            Err(ReadCapped::Transport(e)) => {
                return Placement::Unavailable {
                    name,
                    reason: format!("下载中断（{e}）"),
                };
            }
        }
        return place_in_workspace(remote, r, name, mime, size, workspace, taken).await;
    }

    // ── 岔路 3：其余全部走工作区 ──
    // 响应已在手，直接消费掉省一次往返
    match workspace {
        Some(ws) => write_stream_to_workspace(resp, r, name, mime, ws, taken).await,
        None => Placement::Unavailable {
            name,
            reason: format!("这类内容（{mime}）要放进工作区才能读，而这个会话没有绑定工作区"),
        },
    }
}

/// 图片/文本试读失败后落工作区的那条重取路径。
async fn place_in_workspace(
    remote: &Remote,
    r: &AttachmentRef,
    name: String,
    mime: String,
    size: Option<u64>,
    workspace: Option<&Path>,
    taken: &mut Vec<String>,
) -> Placement {
    let Some(ws) = workspace else {
        return Placement::Unavailable {
            name,
            reason: format!(
                "内容太大（{}），且这个会话没有绑定工作区可放",
                size.map_or_else(|| "未知大小".to_string(), format_bytes),
            ),
        };
    };
    match remote.blob_response(&r.hash).await {
        Ok(resp) => write_stream_to_workspace(resp, r, name, mime, ws, taken).await,
        Err(e) => Placement::Unavailable {
            name,
            reason: format!("取不到内容（{e}）"),
        },
    }
}

/// 把响应体流式落到工作区 `attachments/` 下。
async fn write_stream_to_workspace(
    mut resp: reqwest::Response,
    r: &AttachmentRef,
    name: String,
    mime: String,
    ws: &Path,
    taken: &mut Vec<String>,
) -> Placement {
    if resp
        .content_length()
        .is_some_and(|s| s > WORKSPACE_FILE_MAX)
    {
        return Placement::Unavailable {
            name,
            reason: format!(
                "太大（{}，上限 {}）",
                format_bytes(resp.content_length().unwrap_or(0)),
                format_bytes(WORKSPACE_FILE_MAX)
            ),
        };
    }

    let file_name = unique_file_name(&name, &r.hash, taken);
    let dir = ws.join(ATTACHMENT_DIR);
    let dest = dir.join(&file_name);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return Placement::Unavailable {
            name,
            reason: format!("工作区写不进去（{e}）"),
        };
    }

    // 先写临时名再 rename：模型的文件工具可能在任何时刻来读，
    // 不能让它读到半截文件
    let tmp = dir.join(format!(".{file_name}.part"));
    let written = write_stream(&mut resp, &tmp).await;
    let size = match written {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Placement::Unavailable {
                name,
                reason: format!("下载中断（{e}）"),
            };
        }
    };
    if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Placement::Unavailable {
            name,
            reason: format!("工作区写不进去（{e}）"),
        };
    }
    taken.push(file_name.clone());
    Placement::WorkspaceFile {
        name,
        mime,
        size,
        rel: format!("{ATTACHMENT_DIR}/{file_name}"),
    }
}

async fn write_stream(resp: &mut reqwest::Response, dest: &PathBuf) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        total += chunk.len() as u64;
        // Content-Length 缺失或撒谎时的兜底：写到上限就停
        if total > WORKSPACE_FILE_MAX {
            return Err("超过工作区单文件上限".to_string());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(total)
}

enum ReadCapped {
    TooLarge,
    Transport(String),
}

/// 把响应体读进内存，超过 `cap` 立即中止（不再收剩下的字节）。
async fn read_capped(mut resp: reqwest::Response, cap: u64) -> Result<Vec<u8>, ReadCapped> {
    if resp.content_length().is_some_and(|s| s > cap) {
        return Err(ReadCapped::TooLarge);
    }
    let mut buf: Vec<u8> = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| ReadCapped::Transport(e.to_string()))?
    {
        if buf.len() as u64 + chunk.len() as u64 > cap {
            return Err(ReadCapped::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 渲染「本轮附件」说明块，跟在用户原话后面。
///
/// 图片不在这里重复描述内容 —— 它们已经是消息里的 Image 块；这里只
/// 说明**有哪些**以及各自去了哪，让模型知道「工作区里那个文件就是
/// 用户刚给的东西」。措辞是陈述事实，不是指令 —— 与注入块同一条纪律。
pub fn render_manifest(placements: &[Placement]) -> String {
    if placements.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n[本轮附件]\n");
    for p in placements {
        match p {
            Placement::Image { name, mime, .. } => {
                s.push_str(&format!("- {name}（{mime}）：已作为图片随本条消息附上\n"));
            }
            Placement::InlineText { name, text } => {
                s.push_str(&format!(
                    "- {name}（{} 字符）内容如下：\n```\n{text}\n```\n",
                    text.chars().count()
                ));
            }
            Placement::WorkspaceFile {
                name,
                mime,
                size,
                rel,
            } => {
                s.push_str(&format!(
                    "- {name}（{mime}，{}）：已放入工作区 `{rel}`，可用文件工具读取\n",
                    format_bytes(*size)
                ));
            }
            Placement::Unavailable { name, reason } => {
                s.push_str(&format!("- {name}：本轮不可用 —— {reason}\n"));
            }
        }
    }
    s
}

/// 历史轮次里附件的一行注记。**必须确定性**：同一条 episode 每轮都要
/// 渲染出逐字节相同的文本，否则历史被改写、前缀缓存失效（成本 5-10 倍，
/// 见 `cortex_core::injection` 的教训）。
#[must_use]
pub fn history_note(atts: &[cortex_proto::dto::AttachmentDto]) -> String {
    if atts.is_empty() {
        return String::new();
    }
    let items: Vec<String> = atts
        .iter()
        .map(|a| {
            let name = a
                .filename
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("未命名文件");
            format!("{name}（{}）", a.mime)
        })
        .collect();
    format!("\n[当时附带的文件：{}]", items.join("、"))
}

/// 引用里给模型和界面看的名字。
fn display_name(r: &AttachmentRef) -> String {
    r.filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || format!("附件-{}", &r.hash[..8.min(r.hash.len())]),
            |s| s.to_string(),
        )
}

/// 落盘文件名：去掉路径与控制字符，撞名时加哈希前缀。
///
/// 名字来自用户（`AttachmentRef.filename`），而它会变成**工作区里的
/// 真实路径** —— `../../.ssh/authorized_keys` 这种名字必须在这里死掉，
/// 不能指望上游清洗过。
fn unique_file_name(name: &str, hash: &str, taken: &[String]) -> String {
    let base: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let base = base.trim_matches(['.', ' ', '_']).to_string();
    let base = if base.is_empty() {
        format!("附件-{}", &hash[..8.min(hash.len())])
    } else {
        base
    };
    if !taken.contains(&base) {
        return base;
    }
    // 同名不同内容：哈希前缀消歧。同名同内容也走这里 —— 各是一次独立引用
    let prefixed = format!("{}-{base}", &hash[..8.min(hash.len())]);
    if taken.contains(&prefixed) {
        return format!("{}-{base}", hash);
    }
    prefixed
}

fn is_texty(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-yaml"
                | "application/yaml"
                | "application/toml"
                | "application/x-sh"
                | "application/x-python"
                | "application/csv"
        )
}

fn format_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(filename: Option<&str>) -> AttachmentRef {
        AttachmentRef {
            hash: "deadbeef".repeat(8),
            kind: None,
            filename: filename.map(str::to_string),
        }
    }

    /// 名字会变成工作区里的真实路径 —— 穿越与盘符必须死在这里。
    #[test]
    fn hostile_filenames_cannot_escape_the_attachment_dir() {
        let mut taken = vec![];
        for evil in [
            "../../.ssh/authorized_keys",
            "..\\..\\windows\\system32\\x",
            "/etc/passwd",
            "C:\\boot.ini",
            "a/b/c.txt",
        ] {
            let got = unique_file_name(evil, "aabbccdd", &taken);
            assert!(
                !got.contains('/') && !got.contains('\\') && !got.starts_with('.'),
                "恶意名 {evil:?} 清洗后仍可能越出目录：{got:?}"
            );
            taken.clear();
        }
    }

    #[test]
    fn colliding_names_get_hash_prefixes_not_overwrites() {
        let mut taken = vec![];
        let a = unique_file_name("data.csv", "aaaa1111bbbb", &taken);
        taken.push(a.clone());
        let b = unique_file_name("data.csv", "cccc2222dddd", &taken);
        assert_ne!(a, b, "两个同名附件落成了同一个文件 —— 后到的会盖掉先到的");
        assert!(b.contains("data.csv"), "消歧后应该还认得出原名：{b}");
    }

    #[test]
    fn empty_or_dot_names_fall_back_to_hash() {
        let taken = vec![];
        for bad in ["", "   ", "...", "._ "] {
            let got = unique_file_name(bad, "deadbeefcafe", &taken);
            assert!(
                got.starts_with("附件-"),
                "空名 {bad:?} 应回落到哈希名，实得 {got:?}"
            );
        }
    }

    /// 历史注记必须确定性：同输入同输出，否则改写历史打穿前缀缓存。
    #[test]
    fn history_note_is_deterministic_and_ordered() {
        let atts = vec![
            cortex_proto::dto::AttachmentDto {
                hash: "a".repeat(64),
                kind: Some("image".into()),
                filename: Some("设计稿.png".into()),
                mime: "image/png".into(),
                size_bytes: 100,
            },
            cortex_proto::dto::AttachmentDto {
                hash: "b".repeat(64),
                kind: None,
                filename: None,
                mime: "text/csv".into(),
                size_bytes: 5,
            },
        ];
        let once = history_note(&atts);
        assert_eq!(once, history_note(&atts), "同输入必须同输出");
        assert!(once.contains("设计稿.png（image/png）"));
        assert!(once.contains("未命名文件（text/csv）"));
        assert_eq!(history_note(&[]), "", "无附件不加任何字节");
    }

    #[test]
    fn manifest_covers_every_placement_kind() {
        let ps = vec![
            Placement::Image {
                name: "图.png".into(),
                mime: "image/png".into(),
                bytes: vec![1],
            },
            Placement::InlineText {
                name: "n.md".into(),
                text: "hi".into(),
            },
            Placement::WorkspaceFile {
                name: "d.csv".into(),
                mime: "text/csv".into(),
                size: 48 * 1024 * 1024,
                rel: "attachments/d.csv".into(),
            },
            Placement::Unavailable {
                name: "x.bin".into(),
                reason: "取不到内容".into(),
            },
        ];
        let m = render_manifest(&ps);
        for needle in [
            "已作为图片随本条消息附上",
            "内容如下",
            "attachments/d.csv",
            "本轮不可用",
            "48.0 MiB",
        ] {
            assert!(
                m.contains(needle),
                "说明块缺了一类投放的描述：{needle}\n{m}"
            );
        }
        assert_eq!(render_manifest(&[]), "", "无附件不加任何字节");
    }

    #[test]
    fn texty_covers_code_and_config_but_not_binaries() {
        for yes in [
            "text/plain",
            "text/markdown",
            "application/json",
            "application/toml",
        ] {
            assert!(is_texty(yes), "{yes} 该按文本内联");
        }
        for no in [
            "application/zip",
            "application/pdf",
            "image/png",
            "application/octet-stream",
        ] {
            assert!(!is_texty(no), "{no} 不该按文本内联");
        }
    }

    #[test]
    fn display_name_prefers_filename_then_hash() {
        assert_eq!(display_name(&att(Some("报表.xlsx"))), "报表.xlsx");
        let anon = display_name(&att(None));
        assert!(anon.starts_with("附件-"), "无名附件用哈希前缀：{anon}");
    }
}
