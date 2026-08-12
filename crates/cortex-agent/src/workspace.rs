//! 工作区路径校验。
//!
//! # 为什么这一关必须存在
//!
//! 工作区路径由**客户端**指定 —— 用户在界面上点一个目录，路径经 HTTP 传上来。
//! [`cortex_agent::Sandbox`] 的路径围栏防的是「模型试图逃出工作区」，
//! 它对「工作区本身就是 `C:\Windows`」完全无能为力：那种情况下模型的每一次
//! 读写都在围栏**内**，围栏尽职尽责地放行。
//!
//! 所以边界在这里：什么目录**配当**工作区，是绑定那一刻的问题，
//! 不是访问那一刻的问题。
//!
//! # 拒绝什么
//!
//! 1. **相对路径** —— 相对于谁？解析它的那个进程的工作目录（cortexd 或本地
//!    agent），而点目录的人根本不知道那是哪儿
//!    那是哪儿。歧义在这里的表现是「绑定成功了但绑到了别的地方」，
//!    没有任何提示。
//! 2. **不存在 / 不是目录** —— 绑一个文件上去，之后每个文件工具都会报
//!    一句看不懂的 IO 错误；此刻拒绝，用户还知道自己刚点了什么。
//! 3. **文件系统根与盘符根** —— 「整台机器」不是工作区。
//! 4. **系统目录及其子目录** —— `C:\Windows`、`/etc`、`/usr` 之流。
//!    agent 在这些地方的写操作后果不可回滚。
//! 5. **用户主目录本身** —— 这一条最容易被认为过头，但它挡的是最现实的
//!    事故：主目录里有 `.ssh/id_rsa`、`.aws/credentials`、浏览器 cookie
//!    数据库、各个项目的 `.env`。把主目录当工作区，等于把它们全部接到了
//!    模型上下文的射程内，而用户以为自己只是「选了个方便的起点」。
//!    子目录（`~/Documents`、`~/work`）一律放行 —— 挡的是范围，不是位置。
//!
//! 校验一律在 **canonicalize 之后**做：先解析完符号链接再比较。
//! 否则工作区里一个 `shortcut -> C:\Windows` 就能把上面几条全绕过去。

use std::path::{Path, PathBuf};

use cortex_core::{CortexError, Result, WORKSPACE_PATH_MAX_CHARS};

/// 校验一个客户端给的工作区路径，返回可入库的规范化绝对路径。
///
/// # Errors
/// 任何一条不满足都返回 [`CortexError::Invalid`]，消息里说清是哪一条 ——
/// 客户端要把它直接显示给用户，「非法路径」这种话等于没说。
pub fn validate(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CortexError::Invalid(
            "工作区路径不能为空；要退回纯聊天请把 workspace 显式设为 null".into(),
        ));
    }
    if trimmed.chars().count() > WORKSPACE_PATH_MAX_CHARS {
        return Err(CortexError::Invalid(format!(
            "工作区路径过长（上限 {WORKSPACE_PATH_MAX_CHARS} 字符）"
        )));
    }

    let given = Path::new(trimmed);
    if !given.is_absolute() {
        return Err(CortexError::Invalid(format!(
            "工作区必须是绝对路径，收到的是 {trimmed}；\
             相对路径会相对于解析它的那个进程的工作目录，那不是你以为的位置"
        )));
    }

    // canonicalize 一次做完三件事：确认存在、解析符号链接、消掉 `.` 与 `..`。
    // 后两件是安全相关的 —— 不解析链接就比较，等于比较了一个假身份
    let real = given.canonicalize().map_err(|e| {
        CortexError::Invalid(format!("工作区 {trimmed} 不可用：{e}（路径必须已经存在）"))
    })?;

    if !real.is_dir() {
        return Err(CortexError::Invalid(format!("工作区 {trimmed} 不是目录")));
    }

    // 根目录没有父级。这一条同时挡掉 `/` 与 `C:\`
    if real.parent().is_none() {
        return Err(CortexError::Invalid(format!(
            "{trimmed} 是文件系统根目录，不能作为工作区 —— 「整台机器」不是工作区"
        )));
    }

    if let Some(hit) = forbidden_ancestor(&real) {
        return Err(CortexError::Invalid(format!(
            "{trimmed} 位于系统目录 {} 之下，已拒绝",
            display(&hit)
        )));
    }

    if is_home_root(&real) {
        return Err(CortexError::Invalid(format!(
            "{trimmed} 是用户主目录本身，范围过大 —— \
             它下面有 SSH 私钥、云凭证与各项目的 .env。请选一个子目录"
        )));
    }

    Ok(display(&real))
}

/// 命中的系统目录（若有）。比较的是**已 canonicalize 的两侧**。
fn forbidden_ancestor(real: &Path) -> Option<PathBuf> {
    system_roots()
        .into_iter()
        .find(|root| real.starts_with(root))
}

/// 本机的系统目录清单。
///
/// 优先从环境变量取（`C:\Windows` 并不总在 C 盘，`%ProgramFiles%` 更是随
/// 安装语言变），取不到再用字面量兜底。每一项都 canonicalize 后才进清单：
/// 与被校验路径的比较必须两侧同形，否则 `\\?\` 前缀会让 `starts_with` 永远为假 ——
/// 而那个失败是**静默放行**，最坏的一种。
fn system_roots() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if cfg!(windows) {
        for var in [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ] {
            if let Ok(v) = std::env::var(var)
                && !v.trim().is_empty()
            {
                candidates.push(PathBuf::from(v));
            }
        }
        for lit in [
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\$Recycle.Bin",
        ] {
            candidates.push(PathBuf::from(lit));
        }
    } else {
        for lit in [
            "/etc",
            "/bin",
            "/sbin",
            "/boot",
            "/dev",
            "/proc",
            "/sys",
            "/usr",
            "/lib",
            "/lib64",
            "/var", // macOS
            "/System",
            "/Library",
            "/private",
            "/Applications",
        ] {
            candidates.push(PathBuf::from(lit));
        }
    }

    candidates
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}

/// 这个路径是不是用户主目录**本身**（子目录不算）。
fn is_home_root(real: &Path) -> bool {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var)
        .ok()
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok())
        .is_some_and(|home| home == real)
}

/// 去掉 Windows 的 `\\?\` verbatim 前缀，让入库与回显的路径是人看得懂的形状。
///
/// `canonicalize` 在 Windows 上一律返回 verbatim 形式。原样存下来的话，
/// 客户端界面上会显示 `\\?\D:\codes\cortex`，而用户从资源管理器复制来的是
/// `D:\codes\cortex` —— 两者字符串不等，「这个目录是不是已经绑过了」
/// 这类判断就全错了。去掉前缀不影响后续使用：[`cortex_agent::Sandbox::new`]
/// 自己会再 canonicalize 一次。
fn display(p: &Path) -> String {
    let s = p.to_string_lossy();
    // UNC 的 verbatim 形式是 `\\?\UNC\server\share`，还原成 `\\server\share`
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// 在系统提示词后面接上「你站在哪」。
///
/// # 为什么必须说出这个绝对路径
///
/// 工具描述里写了「相对于工作区根的路径」，但**从没说过那个根是哪个目录**。
/// 于是模型分不清「桌面」与「工作区」是不是同一个地方：用户问桌面上的文件，
/// 它 `list_dir(".")` 看了工作区，然后如实汇报工作区里的内容 ——
/// 中间那步「桌面根本不在我视野里」它没有能力想到。
///
/// 线上观察到的两种失败都不报错：
///
/// * 拿工作区里名字相近的文件顶替用户说的那个（`text.txt` 顶 `test.txt`），
///   读起来像是它真的去桌面看过
/// * 未绑定时承诺「绑个工作区我就能帮你在桌面上建文件」—— 而绑完仍然不能，
///   因为围栏挡的是**范围**，不是位置
///
/// # 为什么放在系统提示词而不是每轮贴一次
///
/// 一个会话的工作区是稳定的，放进可缓存前缀不会逐轮打穿它
/// （见 CLAUDE.md 约束 4）。改绑会让前缀失效一次 —— 那是罕见动作，
/// 换来的是模型每一轮都知道自己站在哪。
///
/// # 为什么在这个 crate 里
///
/// cortexd 与 cortex-local 各有一份 base 提示词，且**已经在漂**
/// （「具备长期记忆的助手」vs「有长期记忆的通用 AI 助理」）。
/// 各自再贴一段边界说明只会漂得更开，而这一段恰恰是两侧必须一字不差的：
/// 同一个会话在桌面端与 Web 上对「桌面能不能碰」答得不一样，
/// 是最难归因的一类差异。
#[must_use]
pub fn brief(base: &str, workspace: Option<&str>) -> String {
    match workspace {
        Some(path) => format!(
            "{base}\n\n当前会话的工作区是 `{path}`。\
             文件工具的路径一律相对于这个目录，**它之外的任何位置你都看不见、\
             也无法操作** —— 桌面、下载、别的项目目录都在外面。\
             用户提到工作区外的位置时，直接说明那不在当前工作区、\
             需要把它绑成工作区才能操作；不要拿工作区里名字相近的文件顶替，\
             也不要承诺你做不到的事。"
        ),
        None => format!(
            "{base}\n\n当前会话没有绑定工作区，因此你没有任何文件工具，\
             可访问的文件范围是空集。用户要求操作文件时，说明需要先给这个会话\
             绑定一个目录，**并且绑定之后能操作的也只有那个目录本身** —— \
             不要让人以为绑完就能碰桌面或整台机器。"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_ordinary_directory() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let got = validate(&dir.path().to_string_lossy()).expect("普通目录应当被接受");
        assert!(
            !got.starts_with(r"\\?\"),
            "入库的路径不该带 verbatim 前缀，否则与用户复制来的字符串对不上：{got}"
        );
        assert!(Path::new(&got).is_dir(), "回来的应当仍是那个目录：{got}");
    }

    #[test]
    fn rejects_relative_paths() {
        let err = validate("./src").expect_err("相对路径必须被拒绝");
        assert!(
            err.to_string().contains("绝对路径"),
            "报错要说清是哪一条，实际：{err}"
        );
    }

    #[test]
    fn rejects_missing_paths() {
        let missing = if cfg!(windows) {
            r"C:\definitely-not-here-9f3a2b"
        } else {
            "/definitely-not-here-9f3a2b"
        };
        assert!(validate(missing).is_err(), "不存在的路径必须被拒绝");
    }

    #[test]
    fn rejects_a_file_that_is_not_a_directory() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").expect("应能写临时文件");
        let err = validate(&file.to_string_lossy()).expect_err("文件不能当工作区");
        assert!(err.to_string().contains("不是目录"), "实际：{err}");
    }

    #[test]
    fn rejects_the_filesystem_root() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        // 根目录既撞「无父级」也可能撞系统目录清单，两条都是拒绝，不必区分
        assert!(validate(root).is_err(), "根目录必须被拒绝");
    }

    #[test]
    fn rejects_system_directories_and_their_children() {
        let cases: &[&str] = if cfg!(windows) {
            &[r"C:\Windows", r"C:\Windows\System32", r"C:\Program Files"]
        } else {
            &["/etc", "/usr", "/usr/bin"]
        };
        for c in cases {
            // 本机上不存在的就跳过 —— 校验器对不存在的路径本来就会拒绝，
            // 那不能证明「系统目录被识别出来了」
            if !Path::new(c).is_dir() {
                continue;
            }
            let err = validate(c).expect_err(&format!("系统目录 {c} 竟然被接受了"));
            assert!(
                err.to_string().contains("系统目录") || err.to_string().contains("根目录"),
                "{c} 应当因系统目录被拒，实际：{err}"
            );
        }
    }

    #[test]
    fn rejects_the_home_directory_itself_but_not_its_children() {
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let Ok(home) = std::env::var(var) else {
            return; // 环境里没有主目录变量（容器里常见），跳过
        };
        let Ok(home) = PathBuf::from(home).canonicalize() else {
            return;
        };

        let err = validate(&home.to_string_lossy()).expect_err("主目录本身必须被拒绝");
        assert!(err.to_string().contains("主目录"), "实际：{err}");

        // 子目录必须放行 —— 挡的是范围，不是位置
        let child = home.join(".cortex-workspace-test");
        if std::fs::create_dir_all(&child).is_ok() {
            let got = validate(&child.to_string_lossy());
            let _ = std::fs::remove_dir(&child);
            assert!(got.is_ok(), "主目录下的子目录不该被拒：{got:?}");
        }
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        for raw in ["", "   ", "\t"] {
            let err = validate(raw).expect_err("空路径必须被拒绝");
            assert!(
                err.to_string().contains("不能为空"),
                "空路径的报错应当引导用户去用 null 解绑，实际：{err}"
            );
        }
    }

    /// 符号链接必须在**解析之后**才参与判定。
    ///
    /// 不解析的话，工作区里一个指向系统目录的链接就能让上面所有检查失效，
    /// 而失效的表现是「一切正常」。
    #[cfg(unix)]
    #[test]
    fn resolves_symlinks_before_judging() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let link = dir.path().join("sneaky");
        if std::os::unix::fs::symlink("/etc", &link).is_err() {
            return;
        }
        assert!(
            validate(&link.to_string_lossy()).is_err(),
            "指向 /etc 的符号链接必须被识破"
        );
    }

    /// 绑定之后，**工作区的绝对路径必须出现在提示词里**。
    ///
    /// 这是线上那次「你问桌面、它答工作区」的直接原因：工具描述说了
    /// 「相对于工作区根」，却从没说根是哪儿，模型没有能力区分两个地方。
    #[test]
    fn bound_brief_names_the_absolute_path() {
        let out = brief("底座", Some(r"D:\work\cortex-workspace"));
        assert!(
            out.contains(r"D:\work\cortex-workspace"),
            "提示词里必须出现工作区的绝对路径，否则模型分不清「桌面」与「工作区」             是不是同一个地方，实际：{out}"
        );
        assert!(
            out.starts_with("底座"),
            "边界说明是**接在**各自的 base 后面，不是替换掉它 ——              两侧的 base 措辞不同且允许不同，实际：{out}"
        );
        assert!(
            out.contains("桌面"),
            "要把最常撞上的那个位置点名说出来。只说「工作区之外」时，             模型仍会把「桌面」当成工作区的别名"
        );
    }

    /// 未绑定时不能许一个绑完也兑现不了的承诺。
    #[test]
    fn unbound_brief_does_not_overpromise() {
        let out = brief("底座", None);
        assert!(
            out.contains("只有那个目录本身"),
            "线上那句是「绑个工作区我就能帮你在桌面上建 test.txt」——              绑完仍然不能，因为围栏挡的是范围。实际：{out}"
        );
    }

    /// 两侧共用同一份，否则同一个问题在桌面端与 Web 上答得不一样。
    #[test]
    fn both_sides_get_byte_identical_boundary_text() {
        let desktop = brief("有长期记忆的通用 AI 助理", Some("/w"));
        let web = brief("具备长期记忆的助手", Some("/w"));
        let boundary = |s: &str| {
            s.split_once(
                "

",
            )
            .expect("应当有边界段")
            .1
            .to_string()
        };
        assert_eq!(
            boundary(&desktop),
            boundary(&web),
            "边界说明必须逐字相同。两侧各写一份的话，「桌面能不能碰」             会随入口给出不同答案 —— 那是最难归因的一类差异"
        );
    }
}
