//! 沙箱策略 —— 允许什么，而不是禁止什么。
//!
//! 内核这两套机制（landlock、Seatbelt）都是**只能加白名单**的：
//! 没有「允许 `$HOME` 但排除 `$HOME/.ssh`」这种写法。所以策略必须一开始
//! 就按「默认全禁 + 逐条放行」来构造，而不是先放行再挖洞。
//!
//! # 放行清单的取舍
//!
//! 这里最容易犯的错误不是放得太宽，是**放得太紧**：
//! 一个连 `cargo build` 都跑不起来的沙箱，用户第一天就会把它关掉，
//! 而**被关掉的沙箱等于没有沙箱**。所以下面的默认清单是按
//! 「让常见构建工具链能跑通」反推出来的，每一条都写清了为什么必须有。
//!
//! 同时守住那条底线：**`$HOME` 本身永远不在可读清单里**。
//! 因为只能加白名单，放行 `$HOME` 就等于放行 `~/.ssh`、`~/.aws`、
//! `~/.config/gh` —— 那正是这一整层要防的东西。要用的缓存目录逐个点名。

use std::path::{Path, PathBuf};

use super::Attended;

/// 网络策略。
///
/// # 为什么默认仍是断网
///
/// 断网会让冷缓存的 `cargo build` / `npm install` 直接失败，这是实打实的代价。
/// 但另一边是**外传**：一条被批准的命令读到什么都能发出去，且这件事在本地
/// 不留任何痕迹。两种失败的可发现性差着量级 —— 构建失败当场就知道，
/// 数据外传可能永远不知道。
///
/// 所以默认留在 [`Denied`](Self::Denied)：**谁忘了设，得到的是关着的那一档**。
///
/// # 但「由调用方按命令抬起来」那句话是错的，代价很大
///
/// 原文写的是「需要联网的那条命令由调用方显式抬到 [`Allowed`](Self::Allowed)」。
/// 那需要一个「这条命令要不要联网」的判定器，而**它从来没被造出来**：
/// `with_network` 在生产代码里一次调用都没有，只有 macOS 的一条测试在用。
///
/// 后果是从第一个沙箱提交（`01c3750`）起，**每一条 `shell` 都跑在无网状态下** ——
/// `socket(AF_INET, …)` 被 seccomp 回 `EPERM`，于是连 DNS 都做不了。
/// 症状极具误导性：报出来的是
/// `Temporary failure in name resolution` / `Could not resolve proxy`，
/// 读起来像**网络坏了**，而不是「我们自己把它关了」。
/// 在云沙箱里更绕 —— 那里连 DNS 服务器都够不着（`internal` 网段本来就该由
/// 代理代劳解析），于是两个原因叠在同一条错误消息上。
///
/// 它藏了这么久是因为 `scripts/sandbox-verify.sh` 全程用 `docker exec` 验证，
/// 而那条路**不经过这层 seccomp**：出网放行清单、私有段防护、403 拒绝理由
/// 全都真的验过，验的却不是 agent 实际跑命令的那条路。
/// 「断言过了不等于那条路走过了」，这个仓库为同一个信号记过一次。
///
/// # 现在由**执行环境**决定，见 [`crate::ExecEnvironment::network_policy`]
///
/// 因为「能不能出网」的真正边界在每个环境里根本不是同一个东西：
/// 容器里是 `internal` 网段 + 出网代理的放行清单，本机上是逐条确认回路。
/// 让每个调用点各自决定，就是让那两个边界各被重新论证一遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// 只留 `AF_UNIX`：进程间管道要用，但它出不了这台机器。
    #[default]
    Denied,
    /// 放开网络。文件系统限制**依然生效**。
    Allowed,
}

/// 一次执行的沙箱策略。
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// 可读可写。工作区、临时目录、构建缓存。
    pub writable_roots: Vec<PathBuf>,
    /// 只读（含执行）。系统目录、工具链。
    pub readable_roots: Vec<PathBuf>,
    pub network: NetworkPolicy,
    /// 这一侧有没有人在场。见 [`Attended`] —— 只在「本机无沙箱」那一支起作用。
    ///
    /// 放在策略里而不是 `prepare` 的参数：它是**部署形态**的属性
    /// （本地 agent 恒 Yes、cortexd 恒 No），不是每次调用现算的。
    /// 做成参数的话每个调用点都要重新决定一次，而那正是会被写错的地方。
    pub attended: Attended,
}

/// 只读放行的系统路径。
///
/// 少一条的后果都很具体：
/// - `/usr` `/bin` `/sbin` `/lib*` `/opt`：可执行文件与动态库，少了连 `sh` 都起不来
/// - `/etc`：`resolv.conf`（DNS）、`ssl/certs`（TLS 根证书）、`passwd`（用户名解析）
/// - `/proc`：几乎所有运行时都要读 `/proc/self/*`；`ptrace` 另由 seccomp 拦住
/// - `/sys`：`nproc` 与各语言运行时探测 CPU 拓扑要读
/// - `/run`：`/run/systemd/resolve/stub-resolv.conf` 是 `/etc/resolv.conf` 的软链目标
const READABLE_SYSTEM_ROOTS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32", "/opt", "/etc", "/proc",
    "/sys", "/run",
];

/// 可读可写放行的字符设备。
///
/// 不整个放行 `/dev`：那里面还有块设备。逐个点名，代价只是一行清单。
/// `/dev/pts` 与 `/dev/ptmx` 是 pty，交互式 shell 判断 isatty 要用。
const WRITABLE_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
];

/// 相对 `$HOME` 的**只读**目录：工具链本体。
///
/// `~/.cargo` 整体只读而不是整体可写，是个有意的细分：
/// `~/.cargo/bin` 放的是可执行文件、`~/.cargo/config.toml` 能指定 linker，
/// 两者任一可写都等于「在工作区外拿到下一次构建的代码执行」。
/// 真正需要写的只有下载缓存，见 [`HOME_WRITABLE_SUBDIRS`]。
const HOME_READABLE_SUBDIRS: &[&str] = &[".cargo", ".rustup", ".nvm", ".pyenv", ".sdkman"];

/// 相对 `$HOME` 的**可读可写**目录：纯下载缓存。
///
/// 判据是「内容本来就是可以随时删掉重下的」。按这个判据，
/// `~/.cargo/registry` 进得来，`~/.cargo/bin` 进不来。
const HOME_WRITABLE_SUBDIRS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".cache",
    ".npm",
    ".yarn/cache",
    ".local/share/pnpm/store",
    ".m2/repository",
    ".gradle/caches",
    ".pub-cache",
    "go/pkg/mod",
    ".deno",
    ".bun/install/cache",
];

impl SandboxPolicy {
    /// 以一个工作区为中心构造默认策略。
    ///
    /// 路径尽量 `canonicalize`：内核的规则匹配是按**解析后**的真实路径做的，
    /// 拿一条没解析的路径去下规则，工作区里一个指向别处的软链就能让规则落空
    /// （或者反过来，让本该放行的目录匹配不上）。解析不了就原样保留 ——
    /// 那通常意味着路径还不存在，后续 [`Self::existing`] 会把它滤掉。
    #[must_use]
    pub fn workspace(root: &Path) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

        let mut writable_roots = vec![root];
        writable_roots.push(PathBuf::from("/tmp"));
        writable_roots.push(PathBuf::from("/var/tmp"));
        writable_roots.extend(WRITABLE_DEVICES.iter().map(PathBuf::from));

        let mut readable_roots: Vec<PathBuf> =
            READABLE_SYSTEM_ROOTS.iter().map(PathBuf::from).collect();

        if let Some(home) = home_dir() {
            readable_roots.extend(HOME_READABLE_SUBDIRS.iter().map(|s| home.join(s)));
            writable_roots.extend(HOME_WRITABLE_SUBDIRS.iter().map(|s| home.join(s)));
        }
        // 显式配置的工具链目录优先于 `$HOME` 猜测：CI 上 CARGO_HOME 常被搬走
        for var in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Ok(p) = std::env::var(var) {
                let p = PathBuf::from(p);
                writable_roots.push(p.join("registry"));
                writable_roots.push(p.join("git"));
                readable_roots.push(p);
            }
        }
        if let Ok(p) = std::env::var("GOMODCACHE") {
            writable_roots.push(PathBuf::from(p));
        }

        Self {
            writable_roots,
            readable_roots,
            network: NetworkPolicy::default(),
            attended: Attended::No,
        }
    }

    /// 什么都不放行的策略 —— 配 [`crate::Sandbox::sealed`] 用。
    ///
    /// 连 `READABLE_SYSTEM_ROOTS` 都不给：那份只读清单存在的唯一理由是让
    /// `sh` 起得来、让 TLS 找得到根证书，而封闭沙箱里根本不会有进程被启动
    /// （[`crate::tools`] 的 shell 分支在拿 cwd 时就已经拒绝了）。
    /// 给一份「用不上但看起来很宽」的清单，只会让下一个读代码的人以为
    /// 未绑定工作区的会话能读 `/etc`。
    #[must_use]
    pub const fn sealed() -> Self {
        Self {
            writable_roots: Vec::new(),
            readable_roots: Vec::new(),
            network: NetworkPolicy::Denied,
            attended: Attended::No,
        }
    }

    /// 追加一个可写根。
    #[must_use]
    pub fn with_writable(mut self, p: impl Into<PathBuf>) -> Self {
        self.writable_roots.push(p.into());
        self
    }

    #[must_use]
    pub fn with_network(mut self, network: NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    /// 声明这一侧有人在场。见 [`Attended`]。
    ///
    /// **只有本地 agent 该调它。** 服务端调了就是「远端的人替一条跑在
    /// 服务器上的命令背书」，而他看不见那台机器上有什么。
    #[must_use]
    pub fn attended(mut self) -> Self {
        self.attended = Attended::Yes;
        self
    }

    /// 过滤出真实存在的可写根。
    ///
    /// 必须过滤：landlock 下规则时要 `open()` 每个路径，路径不存在会让**整条
    /// 规则集构造失败** —— 也就是一个「本机没装 Go」直接导致沙箱起不来，
    /// 然后按默认拒绝策略，所有命令都不能跑。发行版之间目录差异是常态，
    /// 这个过滤是可移植性的全部。
    #[must_use]
    pub fn existing_writable(&self) -> Vec<PathBuf> {
        Self::existing(&self.writable_roots)
    }

    #[must_use]
    pub fn existing_readable(&self) -> Vec<PathBuf> {
        Self::existing(&self.readable_roots)
    }

    fn existing(paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.exists())
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_is_denied_by_default() {
        // 这条断言守的是 NetworkPolicy 文档里那段论证。改默认值必须先改掉它
        let p = SandboxPolicy::workspace(Path::new("."));
        assert_eq!(
            p.network,
            NetworkPolicy::Denied,
            "默认必须断网 —— 外传是不可发现的失败，构建失败是当场可见的失败"
        );
    }

    #[test]
    fn the_workspace_itself_is_writable() {
        let d = tempfile::tempdir().unwrap();
        let p = SandboxPolicy::workspace(d.path());
        let real = d.path().canonicalize().unwrap();
        assert!(
            p.existing_writable().contains(&real),
            "工作区必须可写，否则 agent 什么都做不了：{:?}",
            p.existing_writable()
        );
    }

    #[test]
    fn home_itself_is_never_readable() {
        // 只能加白名单，所以放行 $HOME 等于放行 ~/.ssh。这条是整层的底线
        let Some(home) = home_dir() else { return };
        let p = SandboxPolicy::workspace(Path::new("."));
        for root in p.readable_roots.iter().chain(p.writable_roots.iter()) {
            assert_ne!(
                *root, home,
                "$HOME 被整体放行了 —— ~/.ssh、~/.aws 会跟着一起漏出去"
            );
            assert!(
                !home.starts_with(root) || root == Path::new("/"),
                "{} 是 $HOME 的祖先，等价于放行了整个 $HOME",
                root.display()
            );
        }
    }

    #[test]
    fn ssh_and_cloud_credentials_are_never_reachable() {
        let Some(home) = home_dir() else { return };
        let p = SandboxPolicy::workspace(Path::new("."));
        let all: Vec<_> = p
            .readable_roots
            .iter()
            .chain(p.writable_roots.iter())
            .collect();
        for secret in [".ssh", ".aws", ".config/gh", ".kube", ".gnupg"] {
            let target = home.join(secret);
            assert!(
                !all.iter().any(|root| target.starts_with(root)),
                "{} 落在了某个放行根之下",
                target.display()
            );
        }
    }

    #[test]
    fn cargo_bin_is_not_writable_but_the_registry_cache_is() {
        // 分得这么细的理由：~/.cargo/bin 可写 = 在工作区外拿到下一次构建的代码执行
        let Some(home) = home_dir() else { return };
        let p = SandboxPolicy::workspace(Path::new("."));
        let bin = home.join(".cargo/bin");
        assert!(
            !p.writable_roots.iter().any(|r| bin.starts_with(r)),
            "~/.cargo/bin 不能可写"
        );
        assert!(
            p.writable_roots.contains(&home.join(".cargo/registry")),
            "~/.cargo/registry 必须可写，否则 cargo 连解压 crate 都做不到"
        );
    }

    #[test]
    fn existing_filter_drops_absent_paths() {
        let d = tempfile::tempdir().unwrap();
        let p = SandboxPolicy {
            writable_roots: vec![d.path().to_path_buf(), d.path().join("nope")],
            ..SandboxPolicy::sealed()
        };
        assert_eq!(
            p.existing_writable().len(),
            1,
            "不存在的路径必须被滤掉 —— 否则本机没装 Go 就会让整个沙箱起不来"
        );
    }
}
