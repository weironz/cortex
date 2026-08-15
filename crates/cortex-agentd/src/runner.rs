//! 沙箱容器的生命周期。
//!
//! # 为什么是一条 trait 而不是一个独立进程
//!
//! 要保住的是**接缝**，不是进程。上生产多租户时隔离级别必然要换 ——
//! gVisor 是 [`DockerRunner`] 里的一个参数，Firecracker / E2B 是换一个实现。
//! 收在这条 trait 后面，那天动的是实现；让调用方直接调 docker，那天要动
//! 调用方。**是否顺带把它挪进独立进程，到那时再判断**。
//!
//! 本期不拆进程的理由很实在：落地目标是开发机，信任边界就是你自己。
//!
//! > 公开材料只讲**隔离原语**（Firecracker / gVisor），两家云 agent 的
//! > **控制面**怎么搭是黑盒。所以上面这段是工程判断，不是查到的事实。
//!
//! # 规格写死在实现里，不从入参来
//!
//! [`SandboxRunner::ensure`] 的入参只有 scope 与 spec hash。镜像名、挂载点、
//! 内存 / CPU / PID 上限、网络、capability 全部是 [`DockerRunner`] 里的常量 ——
//! **调用方说不出「把宿主的 `/` 挂进去」**。这一条有测试守着
//! （[`tests::the_spec_is_not_negotiable`]）。
//!
//! 这不是防「cortexd 会写错」，是防「cortexd 被攻破之后能做什么」：它是直接
//! 处理模型输出的那个进程。

use std::collections::HashMap;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountType};
use bollard::query_parameters as qp;
use cortex_core::{CortexError, Result};

/// 沙箱镜像的默认值 —— 开发机上 `just dev` 本地构建出来的那个 tag。
///
/// **仍然不可由调用方指定。** 部署时可以用 `CORTEX_SANDBOX_IMAGE` 换掉
/// （生产节点的镜像来自 ACR，形如
/// `registry.cn-shenzhen.aliyuncs.com/willspace/cortex-sandbox:v0.1.8`），
/// 但那是**进程启动时读一次**的部署配置，与「某个 HTTP 请求能不能挑镜像」
/// 是两回事 —— 后者永远不行，有测试守着。
const DEFAULT_IMAGE: &str = "cortex/sandbox:dev";

/// 换镜像的环境变量。cortexd **不会自己去 pull** —— 镜像必须在
/// `docker images` 里已经存在（节点上由 `cortex-deploy` 脚本拉）。
/// 拉不到的表现是起沙箱时报 `No such image`，不是静默回落。
const IMAGE_ENV: &str = "CORTEX_SANDBOX_IMAGE";

/// 容器 env 里放沙箱令牌的那个变量名。
///
/// 提成常量只为一件事：`spec_with_image` 写它、`token_matches` 读它，
/// 两处必须是同一个名字。写歪了不报错 —— `token_matches` 恒为 false，
/// 于是**每一轮对话都重建一次容器**，而那只表现为「有点慢」。
const TOKEN_ENV: &str = "CORTEX_TOKEN";

/// 容器内 agent 的端口。与 `Dockerfile.sandbox` 的 `EXPOSE` 一致。
const AGENT_PORT: u16 = 8090;

/// 容器名前缀。带前缀是为了 `docker ps` 一眼认得出，也为了 GC 能按名字捞。
const NAME_PREFIX: &str = "cortex-sbx-";

/// 卷名前缀。**卷比容器长命** —— 停容器保留卷，那是用户的工作区。
const VOLUME_PREFIX: &str = "cortex-ws-";

/// 沙箱专用网段。postgres / rustfs **不接进来**，隔离靠拓扑不靠规则。
///
/// 这个网段必须是 `internal` 的。真机实测过（记在 `docs/sandbox.md` 第八节）：
/// 只要容器有默认路由，它就能经 `host.docker.internal` 够到宿主上任何一个
/// 已发布端口 —— **包括别的项目的数据库**，而且宿主那侧改绑 `127.0.0.1`
/// 也拦不住（Docker Desktop 的转发器在虚拟机里，容器到的正是它那一侧）。
const NETWORK: &str = "cortex-sandbox-net";

/// 双宿出口容器在沙箱网段里的名字。沙箱的 `HTTP(S)_PROXY` 指向它。
const EGRESS_HOST: &str = "cortex-egress";

/// 出网代理端口（沙箱侧，只在 internal 网段里可见）。
const EGRESS_PORT: u16 = 3128;

/// 每沙箱内存上限。
///
/// 512 MiB 是照着 2C/3.5G 生产节点算的：cortexd 载入 embedding 模型后常驻
/// 1.03 GiB，节点余量只有 0.5~0.7 GiB。**这个数字与「不承诺 cargo build 级
/// 任务」是同一个决定的两面** —— 2 核 + 512m 跑 rustc 必 OOM。
const MEMORY_BYTES: i64 = 512 * 1024 * 1024;

/// 内存 + swap 总量（cgroup 语义，不是 swap 单独量）。
///
/// 比 memory 多 128 MiB：给沙箱一条「先变慢、后被杀」的缓冲带。
/// 宿主没开 swap 时这个值等效于 memory，不会有副作用。
const MEMORY_SWAP_BYTES: i64 = MEMORY_BYTES + 128 * 1024 * 1024;

/// CPU 上限（1.5 核）。**不得超过 2.0** —— 生产节点只有 2 核，
/// daemon 会直接拒绝更大的值（`docker-compose.prod.yml` 里那个 `cpus=4`
/// 的默认值已经在生产上踩过）。
const CPU_QUOTA: i64 = 150_000; // period 100_000 → 1.5 核
const CPU_PERIOD: i64 = 100_000;

/// 争抢时让位给 Postgres / cortexd（默认 1024）。
const CPU_SHARES: i64 = 256;

/// 进程数上限。
///
/// 超限时容器**不会被杀**，只是 `fork()` 返回 EAGAIN —— 工具层能感知并当成
/// 普通错误上报。256 而不是 100：`npm install` 一次能开上百线程。
const PIDS_LIMIT: i64 = 256;

/// 宿主吃紧时优先杀沙箱，而不是 Postgres / cortexd。
const OOM_SCORE_ADJ: i64 = 500;

/// 文件描述符上限。**必须显式设**：containerd 2.1.5 起默认从 1048576 降到
/// 1024，而 1024 会咬到 esbuild / 文件 watcher / 并行编译，
/// 表现是一堆看不懂的 EMFILE。
const NOFILE_SOFT: i64 = 8192;
const NOFILE_HARD: i64 = 65536;

/// 起完容器等 agent 应答多久。
///
/// 30 秒是给**冷启动**留的：镜像已在本地，但容器里那个 agent 要先跟 cortexd
/// 握一次协议、问一次「我是谁」，两次网络往返。正常情况一秒内就绪。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 探活的间隔。短一点 —— 这段时间用户正对着一个还没开始输出的界面。
const READY_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// `/tmp` 的 tmpfs 大小。**计入 memory 上限**，所以要算进 512m 里。
const TMPFS_SIZE: &str = "size=128m,mode=1777";

/// 卷在容器里的挂载点，也就是被快照的那个路径。
const WORKSPACE_PATH: &str = "/workspace";

/// 导出 tar 的大小上限。
///
/// 512 MiB：卷配额本身还没做（B4），在那之前这个上限同时兼任「别把一个
/// 塞满的卷整个吸进内存」的护栏。**超限是拒绝，不是截断** ——
/// 一份被截断的备份比没有备份更坏，因为它看起来是有的。
const MAX_EXPORT_BYTES: usize = 512 * 1024 * 1024;

/// cortexd 怎么够到一个沙箱。
///
/// # 为什么是枚举而不是「一个 url + 一个可选的头」
///
/// 原本是 `base_url: String` 加 `route_to: Option<String>`，那两个字段**是
/// 耦合的**：`route_to` 有值时 `base_url` 指的是中继而不是沙箱本身。
/// 于是「容器自己的地址 + 一个路由头」这种非法组合是**表达得出来的**，
/// 而它错了不会报错 —— 只是把头发给一个不认识它的对端，然后被忽略。
///
/// 换成枚举之后那个状态构造不出来。这一条对第二个实现尤其要紧：
/// **k8s 那一版只会产出 [`Self::Direct`]**（cortexd 直连 Pod IP 或 Service，
/// 中继那一整层随之消失），它不该被迫去理解一个只属于 Docker Desktop 的机制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxAddr {
    /// 直连：这个地址就是沙箱自己。
    ///
    /// cortexd 与沙箱同在一个网段时（生产：cortexd 也在容器里；将来 k8s）走这条。
    Direct(String),
    /// 经中继：`url` 一个地址服务所有沙箱，转给谁由 `target` 说。
    ///
    /// 只在「cortexd 是宿主进程 + 沙箱网段是 internal」这个组合下需要 ——
    /// 那种拓扑里**已发布端口不生效**（实测，见 `docs/sandbox.md` 第八节），
    /// 于是宿主够不到容器，得有一个双宿的容器代为转发。
    Relay { url: String, target: String },
}

impl SandboxAddr {
    /// 请求该发到哪个地址。
    #[must_use]
    pub fn endpoint(&self) -> &str {
        match self {
            Self::Direct(url) | Self::Relay { url, .. } => url,
        }
    }

    /// 走中继时要带的路由头的值。直连时没有。
    #[must_use]
    pub fn route_target(&self) -> Option<&str> {
        match self {
            Self::Direct(_) => None,
            Self::Relay { target, .. } => Some(target),
        }
    }
}

/// 目录里的一项。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// 目录恒为 0 —— 算目录大小要递归，而这条路径上没人愿意等。
    pub size: i64,
    /// 最后修改时间，Unix 秒。
    ///
    /// 这一项的用处是回答**「这是 agent 刚写的那个吗」** —— 一个几十个文件的
    /// 工作区里，光看名字和大小分不出哪些是这一轮的产物。
    ///
    /// `Option` 而不是 0：tar 里确实可能没有这个字段（`header().mtime()` 返回
    /// `Result`），而 0 是 1970 年，会在界面上显示成一个煞有介事的假日期。
    /// 「不知道」必须能表达成「不知道」。
    pub mtime: Option<i64>,
}

/// 把 docker archive API 给的 tar 解成**一层**目录项。
///
/// 单独拎出来是为了能测：`list_dir` 要连 daemon，而这段解析是纯计算，
/// 而且它有一处**只有构造出特定顺序的 tar 才看得见**的微妙 —— 见函数体里
/// 关于目录自身记录与子项记录的那段。
///
/// # Errors
/// tar 解不开，或者里面的路径不合法。
fn one_level_from_tar(tar: &[u8]) -> Result<Vec<DirEntry>> {
    // archive API 给的是**整个子树**的 tar，而我们只要一层。
    //
    // 为什么不换个 API：docker 没有「列目录」这条。要么这样，要么在容器里
    // 跑一次 `ls` —— 而那是**不可信侧**，它想报什么就报什么
    // （一个被注入的 agent 可以把 `secrets.env` 从列表里藏掉）。
    // 宁可多解一次 tar。
    //
    // 代价：一个巨大的目录会把整棵子树拉过来。上限由 `MAX_EXPORT_BYTES`
    // 兜着，超了报错而不是截断 —— 截断出来的是一份**看起来完整的**
    // 文件列表，那比报错糟得多。
    let mut out: Vec<DirEntry> = Vec::new();
    // name → (在 out 里的下标, 这一项自己那条 tar 记录是否已经见过)
    //
    // 第二个字段是为了 **mtime**：一个目录在 tar 里既有自己那条记录，
    // 也有它每个子项的记录，而两者的 mtime 是不同的东西。谁先出现取决于
    // tar 的组织方式，所以不能「第一条见到的就算」—— 那样同一个目录的
    // 修改时间会随目录内容变化而在两个值之间跳，而且没有任何规律可循。
    // 自己那条记录一旦出现就**覆盖**，且只覆盖一次。
    let mut seen: std::collections::HashMap<String, (usize, bool)> =
        std::collections::HashMap::new();
    let mut ar = tar::Archive::new(std::io::Cursor::new(tar));
    let entries = ar
        .entries()
        .map_err(|e| CortexError::Store(format!("解 tar 失败：{e}")))?;

    for entry in entries {
        let entry = entry.map_err(|e| CortexError::Store(format!("读 tar 项失败：{e}")))?;
        let ep = entry
            .path()
            .map_err(|e| CortexError::Store(format!("tar 里的路径不合法：{e}")))?;
        // tar 里的路径形如 `basename/子路径...`，第一段是被导出的那个目录
        // 自己。要的是**紧邻的下一段**
        let mut segs = ep.components();
        let _root = segs.next();
        let Some(first) = segs.next() else { continue };
        let name = first.as_os_str().to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        // 还有第三段 = 这条记录是**子项**，不是这一项自己
        // （目录自己那条记录可能排在后面，也可能先出现）
        let deeper = segs.next().is_some();
        let is_dir = deeper || entry.header().entry_type().is_dir();
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .and_then(|m| i64::try_from(m).ok());
        let size = if is_dir {
            0
        } else {
            i64::try_from(entry.header().size().unwrap_or(0)).unwrap_or(i64::MAX)
        };

        match seen.get_mut(&name) {
            // 已经收过，而这条是它自己那条记录 —— 用它把之前从子项那儿
            // 猜来的 mtime 换掉
            Some((idx, exact @ false)) if !deeper => {
                *exact = true;
                out[*idx].is_dir = is_dir;
                out[*idx].size = size;
                out[*idx].mtime = mtime;
            }
            Some(_) => {}
            None => {
                seen.insert(name.clone(), (out.len(), !deeper));
                out.push(DirEntry {
                    name,
                    is_dir,
                    size,
                    mtime,
                });
            }
        }
    }
    // 目录在前、同类按名字 —— 与桌面端那棵树的顺序一致
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// 把一个用户给的路径校验成工作区内的绝对路径。
///
/// # 这是这一组端点唯一的栅栏
///
/// 「列目录 / 读文件 / 写文件」三条都由**宿主**执行（docker daemon 直接读写
/// 卷），所以容器内的 landlock 与只读 rootfs **在这条路上一点忙都帮不上** ——
/// 挡住 `../../etc/passwd` 的只有这个函数。
///
/// # 为什么不用 `Path::canonicalize`
///
/// 那会去问**宿主**的文件系统，而这些路径是**容器里**的。宿主上根本没有
/// `/workspace`，canonicalize 直接失败；就算它成功了，得到的也是一个
/// 与要问的那个文件系统无关的答案。
///
/// 所以这里做的是**纯字符串上的路径规范化**：逐段走，`.` 丢掉，`..` 弹栈，
/// 弹到根就是越界。这与 `cortex-agent` 的 `ToolSandbox::resolve` 是同一套
/// 论证（它那边也不依赖内核）。
///
/// # Errors
/// 不是绝对路径、含有 `..` 且弹出了工作区、或者压根不在工作区下。
pub fn validate_ws_path(path: &str) -> Result<String> {
    if !path.starts_with('/') {
        return Err(CortexError::Invalid(format!(
            "路径要用绝对路径（以 {WORKSPACE_PATH} 开头），收到的是：{path}"
        )));
    }
    // Windows 上的调用方可能传 `\` —— 容器里那是合法文件名的一部分，不是分隔符。
    // 不做转换，但要拒掉：静默当成分隔符会让「文件名里带反斜杠」变成越界
    if path.contains('\\') {
        return Err(CortexError::Invalid(
            "路径里不该出现反斜杠 —— 容器是 Linux，`\\` 是文件名的一部分而不是分隔符".into(),
        ));
    }

    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return Err(CortexError::Invalid(format!(
                        "路径跑出了工作区：{path}（`..` 弹过了根）"
                    )));
                }
            }
            s => stack.push(s),
        }
    }
    let normalized = format!("/{}", stack.join("/"));

    // 规范化之后再判前缀。**顺序不能反** —— 先判前缀的话，
    // `/workspace/../etc/passwd` 会因为以 `/workspace` 开头而通过
    let root = WORKSPACE_PATH.trim_end_matches('/');
    if normalized == root {
        return Ok(normalized);
    }
    if !normalized.starts_with(&format!("{root}/")) {
        return Err(CortexError::Invalid(format!(
            "{normalized} 不在工作区 {root} 之内"
        )));
    }
    Ok(normalized)
}

/// 一个跑起来的沙箱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    /// 这个沙箱的名字。
    ///
    /// Docker 下是容器名（`cortex-sbx-{scope}`），k8s 下会是 Pod 名 ——
    /// **调用方只拿它做日志与诊断**，不该拿它去拼任何地址。
    pub name: String,
    /// 怎么够到它。
    pub addr: SandboxAddr,
}

/// 起 / 停 / 查一个用户的沙箱。
///
/// # 实现者契约（第二个实现照这个来）
///
/// 这几条不是「建议」——上面每一层（反代、空闲回收、快照、出网围栏）都建立在
/// 它们之上，而违反其中任何一条**都不会当场报错**：
///
/// 1. **`ensure` 幂等**。cortexd 每一轮对话都调它，重建一次就丢一次容器内的
///    进程状态。已经在跑就原样返回。
/// 2. **`ensure` 返回时里面的 agent 必须已经能应答**，不只是「实例已创建」。
///    Docker 那版为此轮询 `/health`；k8s 那版该等 Pod 的 readiness。
///    差别是几百毫秒的 `connection refused`，而它读起来像「沙箱坏了」。
/// 3. **`stop` 保留工作区**。停的是算力不是数据 —— 那个卷常常是用户唯一一份。
/// 4. **规格不从入参来**。镜像、挂载点、内存 / CPU / PID 上限、网络、
///    capability 全部由实现自己定死。调用方只给 scope 与 spec hash，
///    **说不出「把宿主的 `/` 挂进去」**。这是防「cortexd 被攻破之后能做什么」，
///    不是防「cortexd 会写错」。
/// 5. **出网必须靠拓扑挡住，不能只靠 env**。`HTTP_PROXY` 是引导；真正的边界是
///    「那个网络没有默认路由」。只设 env 的话一个 `curl --noproxy` 就绕开了。
/// 6. **导出 / 写回用 tar**。这一条跨运行时是通的（k8s 的 `cp` 也是 tar 流），
///    所以它留在契约里而不是实现里。
///
/// # 换运行时要动多少
///
/// | | 代价 |
/// |---|---|
/// | Podman | **零代码**：它提供 Docker 兼容 API，`DOCKER_HOST` 指过去即可（bollard 读它） |
/// | containerd / CRI-O | 新实现：两者都没有 Docker API |
/// | Kubernetes | 新实现，而且**几个概念要换**：卷→PVC、internal 网段→NetworkPolicy、archive API→`exec` + tar 流、中继→直连 Pod IP（[`SandboxAddr::Relay`] 那一整层消失） |
///
/// 现在只有 Docker 一个实现，**刻意不预先造第二个** —— 没有真实的第二个部署
/// 目标时，抽象只会把当前这一个的形状焊进契约里。这份文档存在的意义是：
/// 那天来的时候，要保住什么是写下来的。
#[async_trait::async_trait]
pub trait SandboxRunner: Send + Sync {
    /// 确保 scope 的沙箱在跑，返回怎么够得着它。**幂等。**
    ///
    /// `spec_hash` 是环境定义的内容哈希（setup.sh + env + 基镜像 digest）。
    /// v1 不做快照缓存，这个参数只进容器标签供排查用；二期 `snapshot()`
    /// 落地时它就是缓存 tag 的那一半。**现在就留着**是因为加参数会波及
    /// 每一个调用点，而那时正是最不想动调用点的时候。
    async fn ensure(&self, scope: &str, token: &str, spec_hash: &str) -> Result<SandboxHandle>;

    /// 停掉容器，**保留卷**。空闲回收走这条。
    async fn stop(&self, scope: &str) -> Result<()>;

    /// 在跑吗。
    async fn status(&self, scope: &str) -> Result<Option<SandboxHandle>>;

    /// 把 `/workspace` 整个导出成一个 tar。**数据兜底的第一层。**
    ///
    /// 放在 trait 上而不是让调用方拿到 `bollard::Docker` 自己发请求：
    /// 那样等于把「这个 runner 底下是 docker」漏给了每一个调用点，
    /// 而这条 trait 存在的全部理由就是将来能换成 gVisor / Firecracker / E2B。
    ///
    /// 导出**由宿主执行**（docker daemon 直接读卷），容器里的进程既不参与
    /// 也阻止不了 —— 正是这一层需要的性质。
    async fn export_workspace(&self, scope: &str) -> Result<bytes::Bytes>;

    /// 把一个 tar 写回 `/workspace`。
    ///
    /// **叠加，不是替换**：同名文件覆盖，快照里没有而现在有的文件不删。
    /// 理由见 `sandbox_snapshot::restore` 的文档。
    async fn import_workspace(&self, scope: &str, tar: bytes::Bytes) -> Result<()>;

    /// 列一层目录。**只列一层** —— 递归会在 `node_modules` 上卡好几秒。
    ///
    /// `path` 必须在工作区之内，由实现校验（[`validate_ws_path`]）。
    async fn list_dir(&self, scope: &str, path: &str) -> Result<Vec<DirEntry>>;

    /// 读一个文件的字节。
    async fn read_file(&self, scope: &str, path: &str) -> Result<bytes::Bytes>;

    /// 写一个文件。父目录不存在时一并建。
    async fn write_file(&self, scope: &str, path: &str, bytes: bytes::Bytes) -> Result<()>;

    /// 一条「哪个沙箱刚刚有东西被 OOM 杀了」的流。
    ///
    /// 返回 `None` = 这个实现没有这种信号（守望就不启动，而不是空转重试）。
    ///
    /// **必须能报出子进程被杀**，不能只报容器主进程死亡：沙箱里最常见的形态
    /// 是 `python train.py` 吃爆内存被杀，而容器本身还好好活着 ——
    /// 那种情况下 `inspect` 的 `OOMKilled` 一直是 false。
    fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>>;

    /// 这个 scope 的工作区占了多少字节。容器不在时 `None`。
    ///
    /// 用来告警，不用来限制 —— named volume 不受 `--storage-opt` 管
    /// （那个只管容器可写层，而沙箱是只读 rootfs，可写层恒空）。
    async fn workspace_bytes(&self, scope: &str) -> Result<Option<u64>>;

    /// 沙箱容器**回调记忆服务用的那个地址**。
    ///
    /// 与 agentd 自己用的 `CORTEX_MEMORY_URL` 不是一回事，也不该是：前者是
    /// 容器在那张 internal 网上看到的名字，后者是 agentd 从自己这边过去的路。
    ///
    /// 在 trait 上而不是只在 `DockerRunner` 上：换成 gVisor / E2B 之后，
    /// 「容器怎么回来找记忆服务」这个问题照样存在，且照样是它答。
    fn callback(&self) -> &str;

    /// 沙箱**能不能看见** [`Self::callback`] 那个地址。
    ///
    /// # 为什么不是「agentd 去打一下那个地址」
    ///
    /// 第一版就是那么写的，**它是错的**，2026-08-15 用 `docker network
    /// disconnect` 当场证伪：agentd 同时接在 `cortex-sandbox-net` 与
    /// `cortex_default` 两张网上，记忆服务从前者掉下去之后，agentd 走后者
    /// 照样 200，而沙箱（只在前者）已经完全连不上。**多宿主机的 DNS 视角
    /// 不等于单宿容器的视角** —— 一个从错误的位置发出的探测，比没有探测更糟，
    /// 因为它会在故障时给出绿灯。
    ///
    /// 所以这里不打 HTTP，直接问 docker：那个容器到底在不在沙箱那张网上。
    /// 这正是故障本身的定义，不是它的某个相关量。
    async fn callback_visible(&self) -> bool;
}

/// 直连 docker.sock 的实现。
pub struct DockerRunner {
    docker: Docker,
    /// cortexd 自己的地址，作为 `CORTEX_REMOTE` 传进容器。
    remote: String,
    /// 容器与 cortexd 是不是同一个 docker 网络。
    ///
    /// 决定两件事：容器用什么地址回调 cortexd，以及 cortexd 用什么地址反代
    /// 回容器。开发机上 cortexd 跑在宿主进程里（不在容器里），所以是 false ——
    /// 两个方向都经 `cortex-egress` 那个双宿容器。
    same_network: bool,
    /// 反向中继的地址（`cortex-egress` publish 到宿主的那个端口）。
    /// `same_network` 为真时用不上。
    relay: String,
    /// 沙箱镜像。启动时从 [`IMAGE_ENV`] 读一次，之后不变 ——
    /// **请求改不了它**（`spec_*` 全部只读这个字段）。
    image: String,
}

/// 从回调地址里取出主机名 —— 在这套拓扑里它就是记忆服务的**容器名**。
///
/// 不用 `url::Url`：为了一个主机名拖一个依赖不值，而这里的输入形态只有
/// `http://name:port` 一种（compose 写死的）。宁可在遇到别的形态时回 `None`
/// —— 那时健康检查报「看不见」，而那本来就是那种配置下的实情。
///
/// 带端口要去掉，否则永远匹配不上容器名，症状是健康检查恒红。
fn callback_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    (!host.is_empty()).then_some(host)
}

/// 把 [`IMAGE_ENV`] 的原始值解析成镜像名。
///
/// **空串当没设。** `docker run -e CORTEX_SANDBOX_IMAGE=` 与 compose 里
/// `${CORTEX_SANDBOX_IMAGE:-}` 展开出来的都是空串而不是「未设置」，
/// 直接拿去用会让 create 报一句语焉不详的 400。
/// 「空串顶掉默认值」在这个仓库里已经是第三次了，所以单独拎出来给测试打。
fn resolve_image(raw: Option<&str>) -> String {
    raw.filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| DEFAULT_IMAGE.to_owned())
}

impl DockerRunner {
    /// 连本机 docker daemon。
    ///
    /// # Errors
    /// 连不上（没装 / 没权限 / daemon 没起）。**不静默降级**：一个连不上
    /// docker 的 cortexd 起不了任何沙箱，而那该在启动时就说清楚，
    /// 不是等用户点了「新建沙箱会话」才报一句看不懂的错。
    pub fn connect(
        remote: impl Into<String>,
        same_network: bool,
        relay: impl Into<String>,
    ) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| CortexError::Config(format!("连不上 docker daemon：{e}")))?;
        let image = resolve_image(std::env::var(IMAGE_ENV).ok().as_deref());
        tracing::info!(%image, "沙箱镜像");
        Ok(Self {
            docker,
            remote: remote.into(),
            same_network,
            relay: relay.into(),
            image,
        })
    }

    /// 真的握一次手，并确认沙箱镜像在本地。
    ///
    /// [`Self::connect`] 只是造客户端，**不发任何请求** —— socket 是 `/dev/null`、
    /// 没权限、daemon 没起，它一样返回 `Ok`。不 ping 的话这些全部要等到用户
    /// 第一次点「云沙箱」才暴露，而那时的错误信息在日志深处。
    ///
    /// 镜像也一并查：cortexd **不会自己 pull**（节点上的镜像由部署脚本拉），
    /// 少了它的表现是每次起沙箱报 `No such image`，同样只有用户才碰得到。
    ///
    /// 两者都只**报告**不阻断：返回 `Err` 的调用方决定要不要关掉沙箱。
    ///
    /// # Errors
    /// daemon 握手失败，或镜像不在本地。
    pub async fn preflight(&self) -> Result<()> {
        self.docker.version().await.map_err(|e| {
            CortexError::Config(format!(
                "docker daemon 握不上手：{e}。\
                 cortexd 在容器里时要挂 /var/run/docker.sock 并给它那个 socket 的组"
            ))
        })?;
        if !self.image_exists(&self.image).await {
            return Err(CortexError::Config(format!(
                "沙箱镜像 {} 不在本地，而 cortexd 不会自己去 pull。\
                 先 docker pull，或用 {IMAGE_ENV} 指到已经拉下来的那个 tag",
                self.image
            )));
        }
        Ok(())
    }

    /// 不该经出网代理的目的地。
    ///
    /// 除了回环与中继，还必须含**回调地址的主机名** —— 那是容器与 cortexd
    /// 之间的内部流量，不是出网。漏掉它的症状是回调被自己的出网清单 403，
    /// 而错误信息说的是「不在放行清单里」，读起来像配置漏了一条。
    fn no_proxy_list(&self) -> String {
        let host = self
            .remote
            .split("://")
            .nth(1)
            .unwrap_or(&self.remote)
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        if host.is_empty() {
            format!("127.0.0.1,localhost,{EGRESS_HOST}")
        } else {
            format!("127.0.0.1,localhost,{EGRESS_HOST},{host}")
        }
    }

    fn container_name(scope: &str) -> String {
        format!("{NAME_PREFIX}{}", sanitize(scope))
    }

    /// 这个正在跑的容器，认的是不是这把令牌。
    ///
    /// 从它 env 里读回来比对。**不是新增的暴露面**：那把令牌本来就是 cortexd
    /// 自己塞进去的，`docker inspect` 能看见它的人也就能直接控制 daemon。
    ///
    /// 读不到（inspect 失败、env 里没这一条）时回 `false` —— 往「重建」那边
    /// 倒。反过来的话，一个我们说不清认哪把令牌的容器会被当成好的留下来，
    /// 而症状是每一条请求 401。
    async fn token_matches(&self, name: &str, token: &str) -> bool {
        let Ok(info) = self
            .docker
            .inspect_container(name, None::<qp::InspectContainerOptions>)
            .await
        else {
            return false;
        };
        let want = format!("{TOKEN_ENV}={token}");
        info.config
            .and_then(|c| c.env)
            .is_some_and(|env| env.contains(&want))
    }

    fn volume_name(scope: &str) -> String {
        format!("{VOLUME_PREFIX}{}", sanitize(scope))
    }

    /// 删掉一个容器，**不存在也当成功**。
    ///
    /// 恢复用的临时容器要「先删再建」与「用完就删」两次，两次都不该因为
    /// 「本来就没有」而失败 —— 而 bollard 的 404 与真失败长得一样，
    /// 每个调用点各自 `let _ =` 一遍就会把真失败也一起吞了。这里至少留个
    /// debug 日志。
    async fn force_remove(&self, name: &str) {
        if let Err(e) = self
            .docker
            .remove_container(
                name,
                Some(
                    qp::RemoveContainerOptionsBuilder::default()
                        .force(true)
                        .build(),
                ),
            )
            .await
            && !matches!(
                e,
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                }
            )
        {
            tracing::debug!(container = %name, error = %e, "删临时容器失败");
        }
    }

    /// `GET /containers/{id}/archive?path=…` 的字节。
    ///
    /// 导出、列目录、读文件三条都走它 —— 都是「让 daemon 去读那个卷」，
    /// 容器里的进程既不参与也阻止不了。
    ///
    /// 上限**拒绝而不是截断**：一份被截断的 tar 解出来是一份看起来完整的
    /// 文件列表 / 一个看起来完整的备份，而少了什么要到用它的那一刻才知道。
    async fn archive(&self, container: &str, path: &str) -> Result<Vec<u8>> {
        use futures::StreamExt as _;

        let opts = qp::DownloadFromContainerOptionsBuilder::default()
            .path(path)
            .build();
        let mut stream = self.docker.download_from_container(container, Some(opts));
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| CortexError::Store(format!("从 {container} 拉 {path} 失败：{e}")))?;
            if buf.len() + chunk.len() > MAX_EXPORT_BYTES {
                return Err(CortexError::Invalid(format!(
                    "{container} 的 {path} 超过 {} MiB，拒绝给出一份不完整的结果。",
                    MAX_EXPORT_BYTES / 1024 / 1024
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    /// `PUT /containers/{id}/archive?path=…`，**借一个临时容器**。
    ///
    /// 真机上撞出来的：沙箱容器是 `--read-only` 的，而这条端点对只读 rootfs
    /// 的容器**一律 400**（`container rootfs is marked read-only`），
    /// 哪怕要写的目标是一个可写的**卷**。导出那一侧没有这个限制。
    ///
    /// 于是造一个 rootfs 可写、挂同一个卷的容器，解进去，删掉。
    /// **create 完就不 start** —— archive API 对「已创建未启动」的容器照常
    /// 工作，所以那个容器从头到尾没跑过一行代码，无所谓它在哪个网段
    /// （`network_mode: none` 只是把这件事写死）。
    async fn put_archive(
        &self,
        scope: &str,
        dest_dir: &str,
        tar: bytes::Bytes,
        make_parents: bool,
    ) -> Result<()> {
        let helper = format!("{}-restore", Self::container_name(scope));
        // 上一次中途崩了会留下它。先删再建，比「已存在就复用」安全：
        // 复用等于信任一个来历不明的容器的挂载配置
        self.force_remove(&helper).await;

        let spec = ContainerCreateBody {
            image: Some(self.image.clone()),
            // 不会被执行（从不 start）。写一条明确的东西，是为了万一在
            // `docker ps -a` 里看见它时一眼知道它是干什么的
            cmd: Some(vec!["/bin/true".to_owned()]),
            host_config: Some(HostConfig {
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(scope)),
                    target: Some(WORKSPACE_PATH.to_owned()),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                // rootfs **可写** —— 这正是借它的全部理由
                readonly_rootfs: Some(false),
                network_mode: Some("none".to_owned()),
                cap_drop: Some(vec!["ALL".to_owned()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(
                    qp::CreateContainerOptionsBuilder::default()
                        .name(&helper)
                        .build(),
                ),
                spec,
            )
            .await
            .map_err(|e| CortexError::Store(format!("建临时容器失败：{e}")))?;

        // 目标目录可能还不存在（用户往一个新子目录里传文件）。
        // archive API 对不存在的目标回 404，而那条错误读起来像「文件没找到」,
        // 与真相（是**父目录**没找到）差得远。所以先 mkdir 一层。
        //
        // 用一个空目录项的 tar 解到根来建 —— 容器从不 start，跑不了 mkdir
        let mut result = Ok(());
        if make_parents && dest_dir != WORKSPACE_PATH.trim_end_matches('/') {
            let rel = dest_dir.trim_start_matches('/');
            let mut b = tar::Builder::new(Vec::new());
            let mut h = tar::Header::new_gnu();
            h.set_size(0);
            h.set_mode(0o755);
            h.set_mtime(0);
            h.set_entry_type(tar::EntryType::Directory);
            h.set_cksum();
            if b.append_data(&mut h, format!("{rel}/"), std::io::empty())
                .is_ok()
                && let Ok(bytes) = b.into_inner()
            {
                let opts = qp::UploadToContainerOptionsBuilder::default()
                    .path("/")
                    .build();
                // 建目录失败不当作致命：目录可能本来就在，而真正的判据是
                // 下面那次写入成不成
                let _ = self
                    .docker
                    .upload_to_container(&helper, Some(opts), bollard::body_full(bytes.into()))
                    .await;
            }
        }

        if result.is_ok() {
            let opts = qp::UploadToContainerOptionsBuilder::default()
                .path(dest_dir)
                .build();
            result = self
                .docker
                .upload_to_container(&helper, Some(opts), bollard::body_full(tar))
                .await
                .map_err(|e| CortexError::Store(format!("写进 {dest_dir} 失败：{e}")));
        }

        // 无论成败都要收拾：留下一个挂着用户卷的容器，下一次 ensure 看不见它，
        // 而它会一直占着那个卷的引用
        self.force_remove(&helper).await;
        result
    }

    /// 这次该用哪个镜像 —— 需要的话先跑一遍 setup 并缓存。
    ///
    /// **永不失败**：任何一步出问题都回落到基镜像并 warn。一个装不上依赖的
    /// 沙箱仍然是一个能聊天、能读写文件的沙箱，而「因为 setup 挂了所以整个
    /// 会话起不来」是明显更坏的取舍。
    async fn prepare_image(&self, scope: &str) -> String {
        let setup = self.read_setup_script(scope).await;
        let hash = crate::env::spec_hash(setup.as_deref(), &self.image);
        let tag = crate::env::cache_tag(&hash);

        // 命中就直接用。**这里就是全部的「缓存失效」逻辑** ——
        // 改了脚本就是另一个 hash、另一个 tag，查不到，于是重跑。
        // 没有一段「要不要失效」的判断，也就没有它写错的可能
        if self.image_exists(&tag).await {
            tracing::debug!(scope = %scope, %tag, "命中环境缓存");
            return tag;
        }

        let Some(script) = setup else {
            // 没有 setup.sh —— 绝大多数会话就是这样，不该有任何噪声
            return self.image.clone();
        };

        tracing::info!(
            scope = %scope, %tag, bytes = script.len(),
            "第一次见到这份 setup.sh，跑一遍并缓存（最多 {} 分钟）",
            crate::env::SETUP_TIMEOUT.as_secs() / 60
        );
        match self.run_setup_and_commit(scope, &tag).await {
            Ok(()) => {
                self.gc_cache().await;
                tag
            }
            Err(e) => {
                tracing::warn!(
                    scope = %scope, error = %e,
                    "setup.sh 没跑成，这次用基镜像。已经写下的文件都还在，\
                     修好脚本之后下一轮会自动重试"
                );
                self.image.clone()
            }
        }
    }

    /// 从卷里读 `setup.sh`。没有就是 `None`。
    ///
    /// 借一个 create 但不 start 的容器来读 —— 与写文件走同一条路，
    /// 理由也一样：这一刻正式容器还没起来，而卷是先于容器存在的。
    async fn read_setup_script(&self, scope: &str) -> Option<Vec<u8>> {
        let probe = format!("{}-setup-probe", Self::container_name(scope));
        self.force_remove(&probe).await;

        let spec = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["/bin/true".to_owned()]),
            host_config: Some(HostConfig {
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(scope)),
                    target: Some(WORKSPACE_PATH.to_owned()),
                    read_only: Some(true),
                    ..Default::default()
                }]),
                network_mode: Some("none".to_owned()),
                cap_drop: Some(vec!["ALL".to_owned()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        if self
            .docker
            .create_container(
                Some(
                    qp::CreateContainerOptionsBuilder::default()
                        .name(&probe)
                        .build(),
                ),
                spec,
            )
            .await
            .is_err()
        {
            return None;
        }

        let got = self.archive(&probe, crate::env::SETUP_PATH).await;
        self.force_remove(&probe).await;

        let tar = got.ok()?;
        if tar.len() > crate::env::MAX_SETUP_BYTES * 2 {
            tracing::warn!(scope = %scope, "setup.sh 太大，忽略");
            return None;
        }
        let mut ar = tar::Archive::new(std::io::Cursor::new(&tar[..]));
        let mut entry = ar.entries().ok()?.next()?.ok()?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).ok()?;
        (!buf.is_empty() || !tar.is_empty()).then_some(buf)
    }

    async fn image_exists(&self, tag: &str) -> bool {
        self.docker.inspect_image(tag).await.is_ok()
    }

    /// setup 阶段：**可写 rootfs、有网、不发沙箱令牌**，跑完 commit 成 tag。
    ///
    /// 三个「不」各有理由：
    ///
    /// - **可写 rootfs**：只读的话 `apt` / `pip` 根本装不上，而 commit 出来的
    ///   镜像会恒等于基镜像加一个空层（`docker commit` 不含卷数据）——
    ///   那正是调研推翻原方案的第三条。
    /// - **有网**：装依赖当然要联网。这一阶段**不**接沙箱网段，走默认网桥；
    ///   出网 allowlist 是给 agent 阶段的（Codex 的两阶段模型同此）。
    /// - **不发沙箱令牌**：这个容器不跑 agent，不需要任何 cortexd 凭据。
    ///   给了就是白送一把。
    async fn run_setup_and_commit(&self, scope: &str, tag: &str) -> Result<()> {
        let name = format!("{}-setup", Self::container_name(scope));
        self.force_remove(&name).await;

        let spec = ContainerCreateBody {
            image: Some(self.image.clone()),
            // **必须覆盖 ENTRYPOINT。**
            //
            // 镜像的 ENTRYPOINT 是 `sandbox-entrypoint`，它最后
            // `exec cortex-local "$@"` —— 于是只给 `cmd` 的话，真正跑起来的是
            // `cortex-local /bin/sh -ec "..."`，clap 认不出这些参数，退出码 2。
            //
            // 真机第一次跑就是这个，而**回落逻辑让它看起来只是「这次没装上」**：
            // 日志里一句 WARN，用户看到的是「缓存好像没什么用」，
            // 而 setup 其实一次都没成功过。
            entrypoint: Some(vec!["/bin/sh".to_owned(), "-ec".to_owned()]),
            // `-e`：任何一步失败就整体失败，别把一个半装好的环境 commit 下来。
            // 半装好的环境比装不上更坏 —— 它会命中缓存，然后在用的时候才炸
            cmd: Some(vec![format!(
                "cd {WORKSPACE_PATH} && sh {}",
                crate::env::SETUP_PATH
            )]),
            // ── setup 阶段以 root 跑，运行阶段仍然是 uid 10002 ──
            //
            // 装依赖就是要写 `/usr/lib`、`/opt`、apt 的缓存 —— 镜像里那个
            // uid 10002 一个都写不了。第一版没改 user，脚本一碰 `/opt` 就
            // permission denied，而**回落逻辑让它看起来只是「这次没装上」**。
            //
            // 这是不是把口子开大了：**没有**。三条各自成立：
            //
            // - `cap_drop ALL` + `no-new-privileges` 照旧 —— 这里的 root 是
            //   一个没有任何 capability 的 root，出不了容器
            // - 这个容器**不发沙箱令牌**，碰不到 cortexd 的任何一条路由
            // - 它 commit 出来的镜像，运行阶段仍以 uid 10002 起
            //
            // 要承认的一点：`setup.sh` 来自工作区，而工作区是**那个不可信
            // agent 能写的**。所以这条等价于「agent 能在一个一次性的、无网
            // 凭据的、无 capability 的容器里以 root 跑一段脚本」——
            // 而它本来就能在那个容器里跑任意命令。提权的是**容器内的身份**，
            // 不是**容器的边界**，而边界才是这一层的防线。
            user: Some("0:0".to_owned()),
            host_config: Some(HostConfig {
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(scope)),
                    target: Some(WORKSPACE_PATH.to_owned()),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                // **可写** —— 这一阶段的全部意义
                readonly_rootfs: Some(false),
                memory: Some(MEMORY_BYTES),
                memory_swap: Some(MEMORY_SWAP_BYTES),
                pids_limit: Some(PIDS_LIMIT),
                cap_drop: Some(vec!["ALL".to_owned()]),
                security_opt: Some(vec!["no-new-privileges".to_owned()]),
                ..Default::default()
            }),
            ..Default::default()
        };

        self.docker
            .create_container(
                Some(
                    qp::CreateContainerOptionsBuilder::default()
                        .name(&name)
                        .build(),
                ),
                spec,
            )
            .await
            .map_err(|e| CortexError::Store(format!("建 setup 容器失败：{e}")))?;

        let outcome = self.wait_setup(&name).await;
        if outcome.is_ok() {
            // `pause` 再 commit：跑完了进程其实已经退出，这一步是照 Daytona
            // 的形状留着 —— 将来若改成「跑到一半就快照」，没有它会 commit 到
            // 一个写了一半的文件系统
            let _ = self.docker.pause_container(&name).await;
            let (repo, t) = tag.split_once(':').unwrap_or((tag, "latest"));
            let opts = qp::CommitContainerOptionsBuilder::default()
                .container(&name)
                .repo(repo)
                .tag(t)
                .build();
            // ── commit 会**继承被 commit 那个容器的配置** ──
            //
            // 于是不管的话，缓存镜像的 entrypoint 是 `/bin/sh -ec`、cmd 是那句
            // setup 命令、user 是 `0:0` —— 用它起出来的「沙箱」会把 setup 再跑
            // 一遍然后退出，**里面根本没有 agent**，而且是 root。
            //
            // 真机第二次跑就是这样：setup 成功、commit 成功、下一步
            // 「沙箱起来了但查不到地址」。这正是调研里那条「缓存命中但内容不对」
            // 的另一种形态 —— 内容装对了，**入口点错了**。
            //
            // 从基镜像读回这三样再写进 commit 配置，而不是照抄一份常量：
            // 抄的那份会在改 Dockerfile 的那天变成谎话。
            let restored = self.base_runtime_config().await;
            let committed = self
                .docker
                .commit_container(opts, restored)
                .await
                .map_err(|e| CortexError::Store(format!("commit 失败：{e}")));
            let _ = self.docker.unpause_container(&name).await;
            self.force_remove(&name).await;
            committed?;
            return Ok(());
        }
        self.force_remove(&name).await;
        outcome
    }

    /// 起 setup 容器并等它跑完。非 0 退出码算失败。
    async fn wait_setup(&self, name: &str) -> Result<()> {
        use futures::StreamExt as _;

        self.docker
            .start_container(name, None::<qp::StartContainerOptions>)
            .await
            .map_err(|e| CortexError::Store(format!("起 setup 容器失败：{e}")))?;

        let mut wait = self
            .docker
            .wait_container(name, None::<qp::WaitContainerOptions>);
        let waited = tokio::time::timeout(crate::env::SETUP_TIMEOUT, wait.next()).await;

        match waited {
            Err(_) => Err(CortexError::Unavailable(format!(
                "setup.sh 跑了超过 {} 分钟还没结束，已中止",
                crate::env::SETUP_TIMEOUT.as_secs() / 60
            ))),
            Ok(None) => Err(CortexError::Store("setup 容器的等待流意外结束".into())),
            // **bollard 把「非 0 退出」变成一个 Err，不是 Ok(status_code != 0)。**
            //
            // 第一版按后者写，于是脚本失败时落进了下面那个通用分支，日志里只有
            // 一句 `Docker container wait error:` —— 连退出码都没有（docker 的
            // `error` 字段常常是空的）。真机第一次跑就是这样，而那条消息对
            // 「为什么没装上」一个字都没说。
            Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => {
                Err(CortexError::Invalid(format!(
                    "setup.sh 以退出码 {code} 结束。最后几行输出：\n{}",
                    self.tail_logs(name).await
                )))
            }
            Ok(Some(Err(e))) => Err(CortexError::Store(format!("等 setup 容器失败：{e}"))),
            Ok(Some(Ok(_))) => Ok(()),
        }
    }

    /// 基镜像的 entrypoint / cmd / user，用来在 commit 时**盖回去**。
    ///
    /// 读回来而不是写一份常量：常量会在改 `Dockerfile.sandbox` 的那天变成
    /// 谎话，而症状是「缓存镜像起出来的容器行为与基镜像不一样」——
    /// 那个差别没有任何一处会报错。
    async fn base_runtime_config(&self) -> bollard::models::ContainerConfig {
        let mut cfg = bollard::models::ContainerConfig::default();
        if let Ok(img) = self.docker.inspect_image(&self.image).await
            && let Some(c) = img.config
        {
            cfg.entrypoint = c.entrypoint;
            cfg.cmd = c.cmd;
            cfg.user = c.user;
            cfg.working_dir = c.working_dir;
            // env 不盖：沙箱的 env 由 `docker run` 那侧逐条给（令牌、远端地址、
            // 代理），而 setup 阶段的 env 里没有它们。留空即用基镜像那份
        }
        cfg
    }

    /// setup 容器的最后几行输出。**失败时唯一有用的东西。**
    ///
    /// 不给的话，用户看到的是一个退出码和「自己去 docker logs」——
    /// 而那个容器下一秒就被删了，他去不了。
    async fn tail_logs(&self, name: &str) -> String {
        use futures::StreamExt as _;

        let opts = qp::LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .tail("20")
            .build();
        let mut stream = self.docker.logs(name, Some(opts));
        let mut out = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            out.push_str(&chunk.to_string());
            if out.len() > 4096 {
                break;
            }
        }
        if out.trim().is_empty() {
            "（没有输出）".to_owned()
        } else {
            out
        }
    }

    /// 缓存镜像的 LRU 回收。
    ///
    /// **必须自己写**：`docker image prune` 只清 dangling（没有 tag 的），
    /// 而我们的每一个都有 tag —— 不写的话它们只涨不减，几周后磁盘满，
    /// 且看不出是谁占的。
    async fn gc_cache(&self) {
        let mut filters = HashMap::new();
        filters.insert(
            "reference".to_owned(),
            vec![format!("{}:*", crate::env::CACHE_REPO)],
        );
        let opts = qp::ListImagesOptionsBuilder::default()
            .filters(&filters)
            .build();
        let Ok(images) = self.docker.list_images(Some(opts)).await else {
            return;
        };

        // ── 「最后使用时间」从哪来 ──────────────────────────
        //
        // docker 的镜像对象上**没有**这个字段，只有 `created`。按 created 排
        // 的话，一个半年前建好、天天在用的环境镜像会第一个被删，然后下一轮
        // 对话要重跑一次十分钟的 setup。
        //
        // 容器就是使用记录 —— 合成的逻辑与理由见
        // `sandbox_env::catalog_with_last_use`。这里只负责问 docker 要数据。
        let mut containers = Vec::new();
        if let Ok(list) = self
            .docker
            .list_containers(Some(
                qp::ListContainersOptionsBuilder::default()
                    .all(true)
                    .build(),
            ))
            .await
        {
            for c in list {
                if let (Some(image), Some(created)) = (c.image, c.created) {
                    containers.push((image, created));
                }
            }
        }

        let catalog = crate::env::catalog_with_last_use(
            images
                .into_iter()
                .filter_map(|img| {
                    let tag = img.repo_tags.first().cloned()?;
                    Some((tag, img.size, img.created))
                })
                .collect(),
            &containers,
        );

        for tag in crate::env::pick_evictions(catalog, crate::env::CACHE_BUDGET_BYTES) {
            match self
                .docker
                .remove_image(&tag, None::<qp::RemoveImageOptions>, None)
                .await
            {
                Ok(_) => tracing::info!(%tag, "缓存镜像超预算，已回收"),
                // 正在被容器用着就删不掉 —— 那是**对的**，跳过
                Err(e) => tracing::debug!(%tag, error = %e, "缓存镜像没删掉（可能正在用）"),
            }
        }
    }

    /// 轮询容器里的 `/health`，直到它应答或超时。
    ///
    /// 用 `/health` 而不是 docker 的 healthcheck 状态：后者最快也要一个
    /// `interval`（10 秒）才翻成 healthy，而 agent 通常一秒内就绪 ——
    /// 让每一轮对话都白等十秒是不可接受的。
    ///
    /// 超时报错而不是硬着头皮转发：转过去拿到的是 `connection refused`，
    /// 而那条错误在用户那儿读起来是「沙箱坏了」，与真相（起得慢）差很远。
    async fn wait_ready(&self, handle: &SandboxHandle) -> Result<()> {
        let url = format!("{}/health", handle.addr.endpoint());
        let deadline = std::time::Instant::now() + READY_TIMEOUT;
        let probe = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .map_err(|e| CortexError::Config(format!("探活客户端构造失败：{e}")))?;

        let mut attempt = 0u32;
        loop {
            let mut req = probe.get(&url);
            if let Some(name) = handle.addr.route_target() {
                // 走中继时这个头是必须的 —— 少了它中继回 400，而 400 也是
                // 「没就绪」的一种，于是症状变成整整 30 秒的空等
                req = req.header(crate::sandbox_proxy::ROUTE_HEADER, name);
            }
            if req.send().await.is_ok_and(|r| r.status().is_success()) {
                tracing::debug!(sandbox = %handle.name, attempt, "沙箱就绪");
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(CortexError::Unavailable(format!(
                    "沙箱 {} 起来了但 {READY_TIMEOUT:?} 内没有应答。\
                     容器还在（可以 docker logs {} 看它卡在哪），重试会复用它。",
                    handle.name, handle.name
                )));
            }
            attempt += 1;
            tokio::time::sleep(READY_POLL).await;
        }
    }

    /// 容器规格。**纯函数，好测** —— 这份规格是安全边界本身，
    /// 而它写对了没人看得出来，写错了也没人看得出来。
    ///
    /// **镜像是唯一的自由度**，而且只让 setup 缓存那条路用（命中时传缓存
    /// tag）。挂载点、内存 / CPU / PID 上限、网络、capability 全部照旧写死：
    /// 缓存镜像换掉的是「里面装了什么」，不是「它被允许做什么」。
    fn spec_with_image(
        &self,
        scope: &str,
        token: &str,
        spec_hash: &str,
        image: &str,
    ) -> ContainerCreateBody {
        let mut labels = HashMap::new();
        labels.insert("cortex.sandbox.scope".to_owned(), scope.to_owned());
        labels.insert("cortex.sandbox.spec".to_owned(), spec_hash.to_owned());

        let mut tmpfs = HashMap::new();
        tmpfs.insert("/tmp".to_owned(), TMPFS_SIZE.to_owned());

        // 容器回调 cortexd 的地址。同网络时走服务名，否则走宿主别名 ——
        // 而在 internal 网段上，后者**只有经出网代理才到得了**（容器自己没有
        // 默认路由）。所以 `CORTEX_REMOTE` 里的地址必须也在放行清单里。
        let remote = self.remote.clone();
        let proxy = format!("http://{EGRESS_HOST}:{EGRESS_PORT}");

        let env = vec![
            format!("CORTEX_REMOTE={remote}"),
            format!("{TOKEN_ENV}={token}"),
            // 出网一律经双宿代理。**env var 只是引导，网络拓扑才是边界** ——
            // 就算容器里的进程把这几个变量删了，internal 网段上也没有第二条
            // 路可走。两者缺一不可：只有 env 的话一个 `curl --noproxy` 就绕开，
            // 只有拓扑的话正常工具会以「网络不通」失败而不是拿到拒绝理由
            format!("HTTP_PROXY={proxy}"),
            format!("HTTPS_PROXY={proxy}"),
            format!("http_proxy={proxy}"),
            format!("https_proxy={proxy}"),
            // 自己人不走代理：容器内回环、中继本身，以及 **cortexd 自己**。
            //
            // 最后那一条是真机撞出来的。同网段部署（生产形态）下
            // `CORTEX_REMOTE=http://cortexd:8080`，而 `cortexd` 不在
            // `NO_PROXY` 里 ⇒ 回调走出网代理 ⇒ 代理按放行清单 403：
            //
            //   cortexd:8080 不在放行清单里，出网代理拒绝了这次连接
            //
            // 宿主部署那侧碰不到，因为它的回调地址 `host.docker.internal:8080`
            // 恰好在默认放行清单里 —— 于是这个 bug **只在生产拓扑下出现**，
            // 而那正是「宿主上跑 cortexd」那种开发方式测不到的东西。
            //
            // 回调本来就该直连：沙箱与 cortexd 在同一个网段，中间放一个代理
            // 既没有意义，也让「出网清单」这件事的语义变浑（那是给**外网**的）。
            format!("NO_PROXY={}", self.no_proxy_list()),
            format!("no_proxy={}", self.no_proxy_list()),
            // 镜像里已经设了这三个，这里重申是为了让「一个容器到底以什么
            // 身份跑」在 `docker inspect` 里一处可见 —— 排查时不必再去翻
            // 镜像的 ENV
            "CORTEX_EXEC_ENV=container".to_owned(),
            "CORTEX_DEFAULT_WORKSPACE=/workspace".to_owned(),
            format!("CORTEX_LOCAL_BIND=0.0.0.0:{AGENT_PORT}"),
        ];

        // **不再映射端口。**
        //
        // 原来这里给非同网段的情况映一个随机高位端口，让宿主上的 cortexd
        // 直连。网段改成 `internal` 之后那条路物理上不存在了：实测过，
        // 内部网段上的已发布端口宿主 `curl` 连不上（`docker ps` 里那一行
        // 映射照常显示 —— 这正是它难查的地方）。
        //
        // 改由 `cortex-egress` 中继：它同时在内部网段与普通网桥上，
        // 自己的端口 publish 到宿主。见 `status()` 里 `base_url` 的取法。

        ContainerCreateBody {
            image: Some(image.to_owned()),
            env: Some(env),
            labels: Some(labels),
            host_config: Some(HostConfig {
                // 唯一的挂载点。**这就是「调用方说不出把宿主的 / 挂进去」
                // 那句话的兑现处** —— 这个 Vec 是常量结构，入参进不来
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(scope)),
                    target: Some("/workspace".to_owned()),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                readonly_rootfs: Some(true),
                tmpfs: Some(tmpfs),
                init: Some(true),
                cap_drop: Some(vec!["ALL".to_owned()]),
                security_opt: Some(vec!["no-new-privileges:true".to_owned()]),
                memory: Some(MEMORY_BYTES),
                memory_swap: Some(MEMORY_SWAP_BYTES),
                cpu_quota: Some(CPU_QUOTA),
                cpu_period: Some(CPU_PERIOD),
                cpu_shares: Some(CPU_SHARES),
                pids_limit: Some(PIDS_LIMIT),
                oom_score_adj: Some(OOM_SCORE_ADJ),
                ulimits: Some(vec![bollard::models::ResourcesUlimits {
                    name: Some("nofile".to_owned()),
                    soft: Some(NOFILE_SOFT),
                    hard: Some(NOFILE_HARD),
                }]),
                // 生命周期由 cortexd 显式管，不给 OOM crash-loop 的机会
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::NO),
                    maximum_retry_count: None,
                }),
                network_mode: Some(NETWORK.to_owned()),
                // `port_bindings` 与 `extra_hosts` 都刻意留空：
                //
                // - 端口：internal 网段上不生效（见上面那段）
                // - host-gateway：没有默认路由，加了也只是让一个名字**解析
                //   得出但连不上**。而「解析成功、连接失败」比「解析失败」
                //   更难查 —— 它看起来像宿主服务挂了。
                //   容器要够到宿主上的 cortexd，走的是出网代理那条路
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// 容器 / 卷名只允许一小撮字符。
///
/// scope 是 `SandboxScope::key()` 拼出来的（用户 id + 可选的项目 id，
/// 两截都是 ULID 形状），但**不假设**它一定是：项目 id 由客户端生成，
/// 而一个能把别的字符带进容器名的调用方，就能造出撞名或带 docker 特殊
/// 语义的名字。不合法的一律换成 `-`。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[async_trait::async_trait]
impl SandboxRunner for DockerRunner {
    fn callback(&self) -> &str {
        &self.remote
    }

    async fn callback_visible(&self) -> bool {
        let Some(host) = callback_host(&self.remote) else {
            return false;
        };
        let Ok(net) = self.docker.inspect_network(NETWORK, None).await else {
            return false;
        };
        net.containers
            .unwrap_or_default()
            .values()
            .any(|c| c.name.as_deref() == Some(host))
    }

    async fn ensure(&self, scope: &str, token: &str, spec_hash: &str) -> Result<SandboxHandle> {
        let name = Self::container_name(scope);

        // 已经在跑、**而且认的就是这把令牌**，就直接回。
        //
        // **幂等**：cortexd 每一轮对话都会调这个，每轮重建等于每轮丢掉一次
        // 容器里的进程状态。
        //
        // 但「在跑」不够。容器的入站认证认的是它**启动时** env 里那把令牌，
        // 而令牌表在内存里 —— cortexd 一重启就空了，下一轮会签一把新的。
        // 那时容器照常 Up、反代照常连上、然后每一条请求都 401，
        // 而错误信息是「缺少或无效的凭据」，读起来像用户没登录。
        //
        // 真机上撞到过：`just dev-restart` 之后第一句话就是 401，
        // 而它会一直 401 到空闲回收把容器停掉为止 —— 现在那是 **12 小时**。
        if let Some(h) = self.status(scope).await? {
            if self.token_matches(&name, token).await {
                return Ok(h);
            }
            // 不匹配就重建。**不试图把旧令牌捡回来用**：cortexd 重启之后
            // 那些容器本来就该被重新接管，让一个带着旧凭据的容器继续跑
            // 才是问题（见 `sandbox_token` 的模块文档）。
            //
            // 重建是安全的：工作区在卷上，rootfs 是 `--read-only`，
            // 需要持久的东西**全都**在卷里。
            tracing::info!(
                sandbox = %name,
                "容器还在但令牌对不上（多半是 cortexd 重启过），重建它"
            );
        }

        // 停着的同名容器要先删掉：它的 env 里带着**上一把**沙箱令牌，
        // 而那把已经作废了。`docker start` 起来的会是一个认证全 403 的僵尸
        let _ = self
            .docker
            .remove_container(
                &name,
                Some(qp::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        // ── 环境准备：需要的话先跑一遍 setup.sh 并缓存成镜像 ──
        //
        // 返回的是这次要用的镜像 —— 有缓存就是缓存镜像，没有 setup.sh
        // 或者 setup 失败就是基镜像。**失败回落而不是让会话起不来**：
        // 一个装不上依赖的沙箱仍然是一个能聊天、能读写文件的沙箱。
        let image = self.prepare_image(scope).await;

        self.docker
            .create_container(
                Some(qp::CreateContainerOptions {
                    name: Some(name.clone()),
                    ..Default::default()
                }),
                self.spec_with_image(scope, token, spec_hash, &image),
            )
            .await
            .map_err(|e| CortexError::Store(format!("建沙箱容器失败：{e}")))?;

        self.docker
            .start_container(&name, None::<qp::StartContainerOptions>)
            .await
            .map_err(|e| CortexError::Store(format!("起沙箱容器失败：{e}")))?;

        let handle = self
            .status(scope)
            .await?
            .ok_or_else(|| CortexError::Store("沙箱起来了但查不到地址".into()))?;

        // ── 等里面那个 agent 真的能应答 ──
        //
        // **容器 "Up" ≠ agent 就绪。** `start_container` 一返回，容器状态就是
        // running，但里面的 cortex-local 还要连一次 cortexd 做协议握手、
        // 问一次「我是谁」、建状态目录，然后才 bind 端口。这中间的几百毫秒里
        // 反代打过去是 `connection refused` —— 而那条错误看起来像
        // 「沙箱坏了」，实际只是早了一点。
        //
        // 真机第一次跑就撞上了。SWE-ReX 的 `startup_timeout` 与 Kortix
        // 的「内层未就绪回 503 带状态」都是同一件事的两种形态。
        self.wait_ready(&handle).await?;
        Ok(handle)
    }

    async fn stop(&self, scope: &str) -> Result<()> {
        let name = Self::container_name(scope);
        // **只 stop 不 rm 卷**：卷是用户的工作区，删它是另一个动作，
        // 且要走确认。docker stop 默认给 10 秒优雅退出
        self.docker
            .stop_container(&name, None::<qp::StopContainerOptions>)
            .await
            .map_err(|e| CortexError::Store(format!("停沙箱容器失败：{e}")))?;
        Ok(())
    }

    async fn status(&self, scope: &str) -> Result<Option<SandboxHandle>> {
        let name = Self::container_name(scope);
        let info = match self
            .docker
            .inspect_container(&name, None::<qp::InspectContainerOptions>)
            .await
        {
            Ok(i) => i,
            // 没有这个容器 —— 正常状态，不是错误
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(e) => return Err(CortexError::Store(format!("查沙箱容器失败：{e}"))),
        };

        let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        if !running {
            return Ok(None);
        }

        // 同网段（生产：cortexd 也在容器里、也接进这个网段）时直连容器名 ——
        // `internal` 只切断出网，网段**内部**的容器之间照常互通。
        //
        // 否则走中继：它一个地址服务所有沙箱，转给谁由请求头带。
        // 原来这里读的是容器映射出的宿主端口，那条路随 internal 网段一起没了。
        let addr = if self.same_network {
            SandboxAddr::Direct(format!("http://{name}:{AGENT_PORT}"))
        } else {
            SandboxAddr::Relay {
                url: self.relay.clone(),
                target: name.clone(),
            }
        };

        Ok(Some(SandboxHandle { name, addr }))
    }

    async fn export_workspace(&self, scope: &str) -> Result<bytes::Bytes> {
        let tar = self
            .archive(&Self::container_name(scope), WORKSPACE_PATH)
            .await?;
        Ok(bytes::Bytes::from(tar))
    }

    async fn import_workspace(&self, scope: &str, tar: bytes::Bytes) -> Result<()> {
        // **解到 `/` 而不是 `/workspace`。**
        //
        // archive API 导出的 tar，成员路径带 `workspace/` 这一级。解到
        // `/workspace` 会得到 `/workspace/workspace/...` —— 而那**不报错**，
        // 只是文件出现在错的地方，用户看到的是「恢复成功了但什么都没回来」。
        self.put_archive(scope, "/", tar, false).await
    }

    async fn list_dir(&self, scope: &str, path: &str) -> Result<Vec<DirEntry>> {
        let path = validate_ws_path(path)?;
        let tar = self.archive(&Self::container_name(scope), &path).await?;
        one_level_from_tar(&tar)
    }

    async fn read_file(&self, scope: &str, path: &str) -> Result<bytes::Bytes> {
        let path = validate_ws_path(path)?;
        let tar = self.archive(&Self::container_name(scope), &path).await?;

        let mut ar = tar::Archive::new(std::io::Cursor::new(&tar[..]));
        let mut entries = ar
            .entries()
            .map_err(|e| CortexError::Store(format!("解 tar 失败：{e}")))?;
        let Some(entry) = entries.next() else {
            return Err(CortexError::Invalid(format!("{path} 是空的或者不是文件")));
        };
        let mut entry = entry.map_err(|e| CortexError::Store(format!("读 tar 项失败：{e}")))?;
        if entry.header().entry_type().is_dir() {
            return Err(CortexError::Invalid(format!(
                "{path} 是目录，不是文件。整包拿走用 /sandbox/workspace.tar"
            )));
        }
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf)
            .map_err(|e| CortexError::Store(format!("读 {path} 的内容失败：{e}")))?;
        Ok(bytes::Bytes::from(buf))
    }

    fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>> {
        use futures::StreamExt as _;

        let mut filters = HashMap::new();
        // 只要 `oom` 这一种事件。不过滤的话这条流会把这台机器上**每个**容器的
        // 每一次 start / stop / health_status 都送过来
        filters.insert("event".to_owned(), vec!["oom".to_owned()]);
        // 只要容器类的。docker 的 events 还包含 image / volume / network
        filters.insert("type".to_owned(), vec!["container".to_owned()]);

        let opts = qp::EventsOptionsBuilder::default()
            .filters(&filters)
            .build();
        let stream = self.docker.events(Some(opts)).filter_map(|ev| async move {
            let ev = ev.ok()?;
            let name = ev.actor?.attributes?.get("name")?.clone();
            // 只报我们自己的沙箱。这台机器上别的项目的容器 OOM 与我们无关，
            // 而混进日志里会让人以为是沙箱在爆
            name.starts_with(NAME_PREFIX).then_some(name)
        });
        Some(stream.boxed())
    }

    async fn workspace_bytes(&self, scope: &str) -> Result<Option<u64>> {
        let name = Self::container_name(scope);
        if self.status(scope).await?.is_none() {
            return Ok(None);
        }

        // 为什么不 `docker system df -v`：那个报的是**卷的**大小，而它把
        // 每个卷都算一遍，在一台跑着几十个容器的机器上要好几秒；
        // 而且它给的是整卷，我们要的正好也是整卷 —— 但代价不成比例。
        //
        // 为什么不在容器里跑 `du`：那是不可信侧，它想报什么就报什么。
        // 这条告警的全部意义是「在快照静默失败之前说一声」，
        // 让被监视的一方自己报数就没有意义了。
        //
        // 于是用与快照同一条路：让 daemon 导出 tar，数字节。代价是要把整个卷
        // 拉一遍 —— 所以这条 30 分钟才跑一次，而且**超上限时的错误正是我们
        // 想知道的那件事**（卷已经大到快照做不了了）。
        match self.archive(&name, WORKSPACE_PATH).await {
            Ok(tar) => Ok(Some(tar.len() as u64)),
            // 超过 MAX_EXPORT_BYTES 时 `archive` 报 Invalid —— 那不是失败，
            // 那正是「已经超了」这个答案本身。回一个大于任何软限的数
            Err(CortexError::Invalid(_)) => Ok(Some(u64::MAX)),
            Err(e) => Err(e),
        }
    }

    async fn write_file(&self, scope: &str, path: &str, bytes: bytes::Bytes) -> Result<()> {
        let path = validate_ws_path(path)?;
        let (dir, name) = path
            .rsplit_once('/')
            .filter(|(_, n)| !n.is_empty())
            .ok_or_else(|| CortexError::Invalid(format!("{path} 没有文件名")))?;

        // 造一个只含这一个文件的 tar，解到它的父目录。
        //
        // 与 `import_workspace` 走同一条「临时容器」的路，理由也一样：
        // 沙箱容器是只读 rootfs，而 `PUT /archive` 对它一律 400。
        let mut ar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        // 0644：用户传进来的是数据不是程序。给可执行位是在给一个
        // **不可信来源的文件**加上「能被 shell 工具直接跑起来」这条性质
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        ar.append_data(&mut header, name, &bytes[..])
            .map_err(|e| CortexError::Store(format!("造 tar 失败：{e}")))?;
        let tar = ar
            .into_inner()
            .map_err(|e| CortexError::Store(format!("造 tar 失败：{e}")))?;

        self.put_archive(scope, dir, bytes::Bytes::from(tar), true)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> DockerRunner {
        // 不连 daemon：这几条测的是**规格**，而规格是纯计算。
        // 连 daemon 的那半在 docs/sandbox.md 的验证清单里靠真机跑
        DockerRunner {
            docker: Docker::connect_with_local_defaults().expect("构造客户端不需要 daemon 真的在"),
            remote: "http://host.docker.internal:8080".into(),
            same_network: false,
            relay: "http://127.0.0.1:3129".into(),
            image: DEFAULT_IMAGE.to_owned(),
        }
    }

    /// 造一份 tar：`(路径, 是否目录, 大小, mtime)`。
    ///
    /// 顺序**照给的来** —— 这一组测试要验的正是「记录以什么顺序出现」。
    fn tar_of(items: &[(&str, bool, u64, u64)]) -> Vec<u8> {
        use std::io::Read as _;
        let mut b = tar::Builder::new(Vec::new());
        for (path, is_dir, size, mtime) in items {
            let mut h = tar::Header::new_gnu();
            h.set_path(path).expect("路径能写进 tar header");
            h.set_size(if *is_dir { 0 } else { *size });
            h.set_mtime(*mtime);
            h.set_mode(0o644);
            h.set_entry_type(if *is_dir {
                tar::EntryType::Directory
            } else {
                tar::EntryType::Regular
            });
            h.set_cksum();
            // **数据长度必须与 header 里的 size 对得上。**
            // 声明 11 字节却 append 空数据，读的时候会从数据起点往前跳 11 字节，
            // 落进下一条 header 中间 —— 流被读坏，而症状是「第二条记录没生效」，
            // 看起来完全像被测代码的 bug。第一版夹具就是这么写的，
            // 差点让我去改一段本来正确的实现。
            let data = std::io::repeat(b'x').take(if *is_dir { 0 } else { *size });
            b.append(&h, data).expect("追加 tar 项");
        }
        b.into_inner().expect("收尾 tar")
    }

    fn by_name<'a>(v: &'a [DirEntry], name: &str) -> &'a DirEntry {
        v.iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("列表里应该有 {name}，实际有：{v:?}"))
    }

    /// 目录的 mtime 要取**它自己**那条记录，不是碰巧先出现的某个子项。
    ///
    /// docker 的 archive API 不保证顺序。按「第一条见到的就算」写，同一个目录
    /// 的修改时间会随内容变化在两个值之间跳，而这种错**只在特定顺序下出现**
    /// —— 手上真机的那个 tar 恰好是目录在前，测不出来。
    #[test]
    fn a_directorys_mtime_comes_from_its_own_record() {
        let dir_mtime = 1_700_000_000;
        let child_mtime = 1_800_000_000;

        for (label, items) in [
            (
                "目录记录在前",
                vec![
                    ("ws/sub", true, 0, dir_mtime),
                    ("ws/sub/a.txt", false, 11, child_mtime),
                ],
            ),
            (
                "子项记录在前",
                vec![
                    ("ws/sub/a.txt", false, 11, child_mtime),
                    ("ws/sub", true, 0, dir_mtime),
                ],
            ),
        ] {
            let got = one_level_from_tar(&tar_of(&items)).expect("解得开");
            let sub = by_name(&got, "sub");
            assert!(sub.is_dir, "[{label}] sub 是目录");
            assert_eq!(
                sub.mtime,
                Some(dir_mtime.cast_signed()),
                "[{label}] 目录的 mtime 必须来自它自己那条记录（{dir_mtime}），                 不是子项的（{child_mtime}）—— 否则同一个目录的时间会随内容变化乱跳"
            );
            assert_eq!(got.len(), 1, "[{label}] 只列一层，子项不该冒出来");
        }
    }

    /// 目录的 size 恒为 0，普通文件取 tar 里的真实大小。
    #[test]
    fn sizes_and_kinds_survive_either_ordering() {
        let got = one_level_from_tar(&tar_of(&[
            ("ws/a.txt", false, 42, 100),
            ("ws/sub/deep.bin", false, 999, 200),
            ("ws/sub", true, 0, 300),
        ]))
        .expect("解得开");

        assert_eq!(got.len(), 2, "一层里只有 a.txt 与 sub，实际：{got:?}");
        // 目录在前、同类按名字 —— 与桌面端那棵树一致
        assert_eq!(got[0].name, "sub", "目录排在前面");
        assert_eq!(by_name(&got, "a.txt").size, 42);
        assert_eq!(
            by_name(&got, "sub").size,
            0,
            "目录不报大小 —— 算它要递归整棵子树，这条路径上没人愿意等"
        );
    }

    /// mtime 缺失时是 `None`，**不是 0**。
    ///
    /// 0 是 1970-01-01，界面会把它显示成一个煞有介事的假日期，
    /// 而用户没法把它和「真的是 1970 年」区分开。
    #[test]
    fn a_missing_mtime_stays_unknown() {
        // set_mtime(0) 就是 tar 里「没有有意义的时间」那种情况
        let got = one_level_from_tar(&tar_of(&[("ws/x.txt", false, 1, 0)])).expect("解得开");
        assert_eq!(
            by_name(&got, "x.txt").mtime,
            Some(0),
            "0 原样透出 —— 界面那侧负责把它当成「未知」而不是 1970 年，             这里不做任何加工，因为 tar 里 0 与「字段缺失」本来就分不开"
        );
    }

    /// 镜像来自**进程配置**，不来自请求。
    ///
    /// `CORTEX_SANDBOX_IMAGE` 是给部署换 registry 路径用的（节点上的镜像
    /// 来自 ACR），而 `ensure` 的入参里没有、也不该有镜像这一项 ——
    /// 谁哪天给它加一个 `image` 参数，等于把「跑什么代码」的决定权交给调用方。
    ///
    /// 这条同时守住空串：compose 里 `${CORTEX_SANDBOX_IMAGE:-}` 展开出来是
    /// 空串而不是「未设置」，直接用会让 create 报一句语焉不详的 400。
    /// 写令牌的那个变量名，和读回来比对的那个，**必须是同一个**。
    ///
    /// 写歪了不报错：`token_matches` 恒为 false，于是**每一轮对话都重建一次
    /// 容器**（每轮丢一次容器内状态、每轮多花 913 ms），而症状只是「有点慢」。
    ///
    /// 反过来漏掉这条比对的症状更难查：cortexd 一重启，还在跑的那些容器
    /// 认的仍是旧令牌，于是每一条请求 401 —— 而错误文案是
    /// 「缺少或无效的凭据」，读起来像用户没登录。真机上撞到过，
    /// 而且空闲回收从 30 分钟改到 12 小时之后，它会一直 401 半天。
    #[test]
    fn 令牌写进容器与读回比对用的是同一个变量名() {
        let spec = runner().spec_with_image("u1", "tok-abc", "v1", DEFAULT_IMAGE);
        let env = spec.env.expect("规格必须带 env");
        assert!(
            env.iter().any(|e| e == &format!("{TOKEN_ENV}=tok-abc")),
            "容器 env 里必须有 {TOKEN_ENV}=<令牌>，否则 token_matches 永远对不上：\
             症状是每一轮对话都重建一次容器。实际 env：{env:?}"
        );
    }

    #[test]
    fn the_image_comes_from_config_never_from_a_request() {
        for (env, want) in [
            (None, DEFAULT_IMAGE),
            (Some(""), DEFAULT_IMAGE),
            (Some("   "), DEFAULT_IMAGE),
            (
                Some("acr.example.com/ns/cortex-sandbox:v1"),
                "acr.example.com/ns/cortex-sandbox:v1",
            ),
        ] {
            assert_eq!(
                resolve_image(env),
                want,
                "CORTEX_SANDBOX_IMAGE={env:?} 应解析成 {want}，空串必须退回默认值而不是原样用"
            );
        }

        // 规格只读 self.image：换掉字段，spec 就跟着换 —— 也就是说
        // 没有第二处硬编码的镜像名藏在别处
        let mut r = runner();
        r.image = "acr.example.com/ns/cortex-sandbox:v1".into();
        let spec = r.spec_with_image("u1", "tok", "hash", &r.image);
        assert_eq!(
            spec.image.as_deref(),
            Some("acr.example.com/ns/cortex-sandbox:v1"),
            "spec 的镜像必须跟着 runner 的配置走"
        );
    }

    /// **规格不可协商。** 入参只有 scope / token / hash，塞不进第二个挂载。
    ///
    /// 这条守的不是「cortexd 会不会写错」，是「cortexd 被攻破之后能做什么」——
    /// 它是直接处理模型输出的那个进程。如果哪天有人给 `ensure` 加一个
    /// 「挂载点」参数，这条测试不会红（它测的是当前这份规格），但下面
    /// 那条 `the_mount_list_is_a_constant_shape` 会在 review 时挡一道。
    #[test]
    fn the_spec_is_not_negotiable() {
        let r = runner();
        let spec = r.spec_with_image("u1", "tok", "hash", DEFAULT_IMAGE);
        let host = spec.host_config.expect("必须有 HostConfig");

        assert_eq!(
            spec.image.as_deref(),
            Some(DEFAULT_IMAGE),
            "镜像名来自 runner 的配置，调用方指定不了"
        );

        let mounts = host.mounts.expect("必须有挂载");
        assert_eq!(
            mounts.len(),
            1,
            "有且只有一个挂载点。多一个就是多一条穿透容器边界的路"
        );
        assert_eq!(mounts[0].target.as_deref(), Some("/workspace"));
        assert_eq!(
            mounts[0].typ,
            Some(MountType::VOLUME),
            "必须是 named volume 而不是 bind —— bind 能指向宿主的任意路径"
        );
        assert_eq!(mounts[0].read_only, Some(false), "工作区要可写");

        assert_eq!(
            host.readonly_rootfs,
            Some(true),
            "rootfs 只读，写路径全收敛到卷"
        );
        assert_eq!(host.cap_drop.as_deref(), Some(&["ALL".to_owned()][..]));
        assert_eq!(
            host.security_opt.as_deref(),
            Some(&["no-new-privileges:true".to_owned()][..])
        );
        assert_eq!(host.memory, Some(MEMORY_BYTES));
        assert_eq!(
            host.pids_limit,
            Some(PIDS_LIMIT),
            "没有它一个 fork 炸弹就能拖死整机"
        );
        assert_eq!(
            host.init,
            Some(true),
            "shell 工具会留僵尸进程，没有 init 会撞 pids 上限"
        );
        assert_eq!(
            host.network_mode.as_deref(),
            Some(NETWORK),
            "必须落在沙箱网段 —— postgres / rustfs 不在那儿，隔离靠拓扑"
        );
    }

    /// CPU 上限不得超过 2 核 —— 生产 daemon 会直接拒绝。
    #[test]
    fn the_cpu_cap_fits_the_production_node() {
        let cores = CPU_QUOTA as f64 / CPU_PERIOD as f64;
        assert!(
            cores <= 2.0,
            "{cores} 核超过生产节点的 2 核，daemon 会拒绝创建容器（已实测踩过）"
        );
    }

    /// tmpfs 计入内存上限，两者要对得上。
    #[test]
    fn the_tmpfs_is_accounted_for_in_the_memory_cap() {
        assert!(
            TMPFS_SIZE.contains("size="),
            "tmpfs 必须限大小：它计入 memory 上限，不限的话一条 dd 就能把\
             整个沙箱的内存吃光，而表现是 OOM 而不是磁盘满"
        );
        // memory-swap 要略大于 memory（给一条「先变慢、后被杀」的缓冲带）。
        // 这一条由 `MEMORY_SWAP_BYTES` 的定义式保证，编译期就成立 ——
        // 写成运行期断言会被 clippy 判成常量断言，且它也确实测不到什么
        const _: () = assert!(MEMORY_SWAP_BYTES > MEMORY_BYTES);
    }

    /// **路径校验是这一组文件端点唯一的栅栏。**
    ///
    /// 列 / 读 / 写三条都由宿主执行（docker daemon 直接读写卷），
    /// 所以容器内的 landlock 与只读 rootfs 在这条路上一点忙都帮不上。
    #[test]
    fn a_path_cannot_escape_the_workspace() {
        for good in [
            "/workspace",
            "/workspace/",
            "/workspace/a.txt",
            "/workspace/sub/deep/x",
            "/workspace/./a.txt",
            "/workspace/sub/../a.txt",
        ] {
            assert!(
                validate_ws_path(good).is_ok(),
                "{good} 是工作区内的合法路径，却被拒了 —— 用户会看到「路径不合法」\
                 而完全不知道哪里不合法"
            );
        }

        for bad in [
            "/etc/passwd",
            "/workspace/../etc/passwd",
            "/workspace/../../root/.ssh/id_rsa",
            "/workspace/a/../../etc/shadow",
            "..",
            "a.txt",               // 相对路径
            "/workspace\\..\\etc", // 反斜杠
            "/",
        ] {
            assert!(
                validate_ws_path(bad).is_err(),
                "{bad} 跑出了工作区却被放行 —— 这条路上没有第二道栅栏：\
                 请求是 cortexd 用宿主身份执行的，容器的 landlock 与只读 rootfs \
                 一个都不参与"
            );
        }
    }

    /// 规范化必须在判前缀**之前**。
    #[test]
    fn normalisation_happens_before_the_prefix_check() {
        // 这一条单独立出来，是因为反过来写**看起来完全正确**：
        // 「先确认它以 /workspace 开头，再规范化」—— 而
        // `/workspace/../etc/passwd` 正是以 /workspace 开头的
        assert!(
            validate_ws_path("/workspace/../etc/passwd").is_err(),
            "先判前缀再规范化的话这条会通过 —— 它确实以 /workspace 开头"
        );
        assert_eq!(
            validate_ws_path("/workspace/sub/../a.txt").expect("弹回工作区内应当放行"),
            "/workspace/a.txt",
            "返回的必须是**规范化之后**的路径：把原样的字符串交给 docker 的话，\
             同一个文件会有多种写法，而缓存与日志按字符串区分"
        );
    }

    /// 寻址方式与「要不要带路由头」**是同一件事的两面**。
    ///
    /// 这条守的是那次重构本身：原先是 `base_url` + `Option<route_to>` 两个
    /// 字段，能表达出「容器自己的地址 + 一个路由头」这种非法组合 ——
    /// 而它错了不报错，只是把头发给一个不认识它的对端然后被忽略。
    ///
    /// 谁哪天为了图方便把它拆回两个字段，这条会红。
    #[test]
    fn addressing_and_routing_cannot_disagree() {
        let direct = SandboxAddr::Direct("http://cortex-sbx-u1:8090".into());
        assert_eq!(direct.endpoint(), "http://cortex-sbx-u1:8090");
        assert_eq!(
            direct.route_target(),
            None,
            "直连时带路由头是没有意义的 —— 对端是沙箱自己，它不认这个头"
        );

        let relay = SandboxAddr::Relay {
            url: "http://127.0.0.1:3129".into(),
            target: "cortex-sbx-u1".into(),
        };
        assert_eq!(relay.endpoint(), "http://127.0.0.1:3129");
        assert_eq!(
            relay.route_target(),
            Some("cortex-sbx-u1"),
            "走中继时**必须**带头：中继一个地址服务所有沙箱，\
             少了它只会收到 400，而 400 在探活那条路上会被当成「还没就绪」，\
             症状是整整 30 秒的空等"
        );
    }

    /// 容器名与卷名只含安全字符。
    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize("01ABC-xyz_9"), "01ABC-xyz_9");
        assert_eq!(
            sanitize("../../etc"),
            "------etc",
            "scope 里的路径字符必须被拍平 —— 带进容器名就能造出撞名或\
             带 docker 特殊语义的名字"
        );
        assert!(DockerRunner::container_name("u1").starts_with(NAME_PREFIX));
        assert!(DockerRunner::volume_name("u1").starts_with(VOLUME_PREFIX));
    }

    /// 沙箱**一个端口都不往宿主映**。
    ///
    /// 这条测试原来是反的（「宿主直连模式必须映射端口」）。反过来是因为
    /// 网段改成了 `internal`，而实测下来内部网段上的已发布端口根本不生效 ——
    /// 宿主 `curl` 连不上，可 `docker ps` 里那一行映射照常显示。
    /// **留着一个不生效的映射比没有更糟**：它让人以为那条路还在。
    ///
    /// 而假如哪天网段改回非 internal，这个映射就会真的生效 —— 那时它是一个
    /// 「能执行命令的容器」对宿主敞开的端口。两头都不该有它。
    /// 回调地址 → 容器名。**端口必须去掉** —— 留着的话永远匹配不上，
    /// 而症状是健康检查恒红，看起来像网真的断了。
    #[test]
    fn callback_host_strips_scheme_and_port() {
        assert_eq!(
            callback_host("http://cormex-cortexd:8080"),
            Some("cormex-cortexd")
        );
        assert_eq!(
            callback_host("http://cormex-cortexd"),
            Some("cormex-cortexd")
        );
        assert_eq!(
            callback_host("https://mem:8080/some/path"),
            Some("mem"),
            "路径要切掉，否则容器名里会混进 /some"
        );
        assert_eq!(callback_host(""), None, "空串不该被当成一个叫「」的容器");
        assert_eq!(
            callback_host("http://"),
            None,
            "只有 scheme 时回 None —— 让健康检查报「看不见」，那是实情"
        );
    }

    #[test]
    fn the_sandbox_publishes_no_ports_at_all() {
        let r = runner();
        let spec = r.spec_with_image("u1", "tok", "hash", DEFAULT_IMAGE);
        let host = spec.host_config.expect("必须有 HostConfig");
        assert!(
            host.port_bindings.is_none(),
            "沙箱不该往宿主映任何端口。cortexd 经 cortex-egress 的反向中继进来，\
             那条路只有一个入口、且中继自己就在信任边界上。实际：{:?}",
            host.port_bindings
        );
        assert!(
            host.extra_hosts.is_none(),
            "也不该加 host-gateway：internal 网段上它只会让一个名字**解析得出\
             但连不上**，而「解析成功、连接失败」比「解析失败」更难查。实际：{:?}",
            host.extra_hosts
        );
    }

    /// 出网一律经代理，且**四个大小写形式都要设**。
    ///
    /// curl 认小写 `http_proxy`，多数语言的 SDK 认大写 —— 只设一半的症状是
    /// 「shell 里 curl 能出去、python 脚本出不去」，或者反过来，
    /// 而两者都不会说这是代理的事。
    #[test]
    fn every_egress_env_var_is_set() {
        let r = runner();
        let spec = r.spec_with_image("u1", "tok", "hash", DEFAULT_IMAGE);
        let env = spec.env.expect("必须有 env");
        let want = format!("http://{EGRESS_HOST}:{EGRESS_PORT}");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert!(
                env.contains(&format!("{key}={want}")),
                "少了 {key} —— 出网会有一路绕开代理。实际 env：{env:?}"
            );
        }
        let no_proxy = env
            .iter()
            .find(|e| e.starts_with("NO_PROXY="))
            .expect("回环与中继自己不该走代理，否则容器内自检会绕一圈甚至打成回环");
        // **回调地址的主机名必须在里面。** 少了它，同网段部署下容器回调
        // cortexd 会被自己的出网清单 403，而错误信息说的是「不在放行清单里」——
        // 读起来像配置漏了一条，其实是把内部流量当成了出网。
        //
        // 宿主部署那侧碰不到（`host.docker.internal:8080` 恰好在默认清单里），
        // 所以这条只在生产拓扑下才会咬人。真机撞过。
        assert!(
            no_proxy.contains("host.docker.internal"),
            "回调地址的主机名不在 NO_PROXY 里：{no_proxy}"
        );
    }
}
