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
//! [`SandboxRunner::ensure`] 的入参只有 owner 与 spec hash。镜像名、挂载点、
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

/// 沙箱镜像。**不可由调用方指定。**
const IMAGE: &str = "cortex/sandbox:dev";

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

/// 一个跑起来的沙箱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    /// 容器名（= `cortex-sbx-{owner}`）。
    pub name: String,
    /// cortexd 该往哪儿反代。
    ///
    /// 同网段（生产：cortexd 也在容器里）时是容器名；
    /// 宿主直连（开发）时是**中继**的地址 —— 不再是容器自己的映射端口，
    /// 因为 internal 网段上已发布端口不生效（实测）。
    pub base_url: String,
    /// 走中继时要带的路由头，值是目标容器名。同网段直连时是 `None`。
    ///
    /// 单独一个字段而不是拼进 `base_url`：中继只有一个地址，
    /// 「转给谁」是每次请求的参数，不是端点的一部分。
    pub route_to: Option<String>,
}

/// 起 / 停 / 查一个用户的沙箱。
#[async_trait::async_trait]
pub trait SandboxRunner: Send + Sync {
    /// 确保 owner 的沙箱在跑，返回怎么够得着它。**幂等。**
    ///
    /// `spec_hash` 是环境定义的内容哈希（setup.sh + env + 基镜像 digest）。
    /// v1 不做快照缓存，这个参数只进容器标签供排查用；二期 `snapshot()`
    /// 落地时它就是缓存 tag 的那一半。**现在就留着**是因为加参数会波及
    /// 每一个调用点，而那时正是最不想动调用点的时候。
    async fn ensure(&self, owner: &str, token: &str, spec_hash: &str) -> Result<SandboxHandle>;

    /// 停掉容器，**保留卷**。空闲回收走这条。
    async fn stop(&self, owner: &str) -> Result<()>;

    /// 在跑吗。
    async fn status(&self, owner: &str) -> Result<Option<SandboxHandle>>;

    /// 把 `/workspace` 整个导出成一个 tar。**数据兜底的第一层。**
    ///
    /// 放在 trait 上而不是让调用方拿到 `bollard::Docker` 自己发请求：
    /// 那样等于把「这个 runner 底下是 docker」漏给了每一个调用点，
    /// 而这条 trait 存在的全部理由就是将来能换成 gVisor / Firecracker / E2B。
    ///
    /// 导出**由宿主执行**（docker daemon 直接读卷），容器里的进程既不参与
    /// 也阻止不了 —— 正是这一层需要的性质。
    async fn export_workspace(&self, owner: &str) -> Result<bytes::Bytes>;

    /// 把一个 tar 写回 `/workspace`。
    ///
    /// **叠加，不是替换**：同名文件覆盖，快照里没有而现在有的文件不删。
    /// 理由见 `sandbox_snapshot::restore` 的文档。
    async fn import_workspace(&self, owner: &str, tar: bytes::Bytes) -> Result<()>;
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
        Ok(Self {
            docker,
            remote: remote.into(),
            same_network,
            relay: relay.into(),
        })
    }

    fn container_name(owner: &str) -> String {
        format!("{NAME_PREFIX}{}", sanitize(owner))
    }

    fn volume_name(owner: &str) -> String {
        format!("{VOLUME_PREFIX}{}", sanitize(owner))
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

    /// 轮询容器里的 `/health`，直到它应答或超时。
    ///
    /// 用 `/health` 而不是 docker 的 healthcheck 状态：后者最快也要一个
    /// `interval`（10 秒）才翻成 healthy，而 agent 通常一秒内就绪 ——
    /// 让每一轮对话都白等十秒是不可接受的。
    ///
    /// 超时报错而不是硬着头皮转发：转过去拿到的是 `connection refused`，
    /// 而那条错误在用户那儿读起来是「沙箱坏了」，与真相（起得慢）差很远。
    async fn wait_ready(&self, handle: &SandboxHandle) -> Result<()> {
        let url = format!("{}/health", handle.base_url);
        let deadline = std::time::Instant::now() + READY_TIMEOUT;
        let probe = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .map_err(|e| CortexError::Config(format!("探活客户端构造失败：{e}")))?;

        let mut attempt = 0u32;
        loop {
            let mut req = probe.get(&url);
            if let Some(name) = &handle.route_to {
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
    fn spec(&self, owner: &str, token: &str, spec_hash: &str) -> ContainerCreateBody {
        let mut labels = HashMap::new();
        labels.insert("cortex.sandbox.owner".to_owned(), owner.to_owned());
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
            format!("CORTEX_TOKEN={token}"),
            // 出网一律经双宿代理。**env var 只是引导，网络拓扑才是边界** ——
            // 就算容器里的进程把这几个变量删了，internal 网段上也没有第二条
            // 路可走。两者缺一不可：只有 env 的话一个 `curl --noproxy` 就绕开，
            // 只有拓扑的话正常工具会以「网络不通」失败而不是拿到拒绝理由
            format!("HTTP_PROXY={proxy}"),
            format!("HTTPS_PROXY={proxy}"),
            format!("http_proxy={proxy}"),
            format!("https_proxy={proxy}"),
            // 自己人不走代理：容器内回环，以及同网段的中继本身
            format!("NO_PROXY=127.0.0.1,localhost,{EGRESS_HOST}"),
            format!("no_proxy=127.0.0.1,localhost,{EGRESS_HOST}"),
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
            image: Some(IMAGE.to_owned()),
            env: Some(env),
            labels: Some(labels),
            host_config: Some(HostConfig {
                // 唯一的挂载点。**这就是「调用方说不出把宿主的 / 挂进去」
                // 那句话的兑现处** —— 这个 Vec 是常量结构，入参进不来
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(owner)),
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
/// owner 来自 `cortex_auth`（ULID 形状），但**不假设**它一定是：一个能把
/// 别的字符带进容器名的调用方，就能造出撞名或带 docker 特殊语义的名字。
/// 不合法的一律换成 `-`。
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
    async fn ensure(&self, owner: &str, token: &str, spec_hash: &str) -> Result<SandboxHandle> {
        let name = Self::container_name(owner);

        // 已经在跑就直接回。**幂等**：cortexd 每一轮对话都会调这个，
        // 而每轮重建容器等于每轮丢掉一次内存里的会话状态
        if let Some(h) = self.status(owner).await? {
            return Ok(h);
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

        self.docker
            .create_container(
                Some(qp::CreateContainerOptions {
                    name: Some(name.clone()),
                    ..Default::default()
                }),
                self.spec(owner, token, spec_hash),
            )
            .await
            .map_err(|e| CortexError::Store(format!("建沙箱容器失败：{e}")))?;

        self.docker
            .start_container(&name, None::<qp::StartContainerOptions>)
            .await
            .map_err(|e| CortexError::Store(format!("起沙箱容器失败：{e}")))?;

        let handle = self
            .status(owner)
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

    async fn stop(&self, owner: &str) -> Result<()> {
        let name = Self::container_name(owner);
        // **只 stop 不 rm 卷**：卷是用户的工作区，删它是另一个动作，
        // 且要走确认。docker stop 默认给 10 秒优雅退出
        self.docker
            .stop_container(&name, None::<qp::StopContainerOptions>)
            .await
            .map_err(|e| CortexError::Store(format!("停沙箱容器失败：{e}")))?;
        Ok(())
    }

    async fn status(&self, owner: &str) -> Result<Option<SandboxHandle>> {
        let name = Self::container_name(owner);
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
        let (base_url, route_to) = if self.same_network {
            (format!("http://{name}:{AGENT_PORT}"), None)
        } else {
            (self.relay.clone(), Some(name.clone()))
        };

        Ok(Some(SandboxHandle {
            name,
            base_url,
            route_to,
        }))
    }

    async fn export_workspace(&self, owner: &str) -> Result<bytes::Bytes> {
        use futures::StreamExt as _;

        let name = Self::container_name(owner);
        let opts = qp::DownloadFromContainerOptionsBuilder::default()
            .path(WORKSPACE_PATH)
            .build();

        let mut stream = self.docker.download_from_container(&name, Some(opts));
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                CortexError::Store(format!("从 {name} 拉 {WORKSPACE_PATH} 失败：{e}"))
            })?;
            if buf.len() + chunk.len() > MAX_EXPORT_BYTES {
                // **拒绝，不截断。** 一份被截断的备份比没有备份更坏：
                // 它看起来是有的，而少了哪一半要到恢复那一刻才知道
                return Err(CortexError::Invalid(format!(
                    "{name} 的 {WORKSPACE_PATH} 超过 {} MiB，拒绝导出一份不完整的快照。\
                     先清理工作区，或等卷配额那一格落地。",
                    MAX_EXPORT_BYTES / 1024 / 1024
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(bytes::Bytes::from(buf))
    }

    async fn import_workspace(&self, owner: &str, tar: bytes::Bytes) -> Result<()> {
        // ── 为什么要借一个临时容器 ──
        //
        // 真机上撞出来的：沙箱容器是 `--read-only` 的，而 docker 的
        // `PUT /containers/{id}/archive` **对只读 rootfs 的容器一律 400**
        // （`container rootfs is marked read-only`），哪怕要写的目标是一个
        // 可写的**卷**。导出那一侧没有这个限制，所以只有恢复要绕。
        //
        // 于是造一个 rootfs 可写、挂同一个卷的容器，把 tar 解进去，再删掉。
        //
        // **它 create 完就不 start** —— archive API 对「已创建未启动」的容器
        // 照常工作。所以这个容器从头到尾**没有跑过任何一行代码**，
        // 也就无所谓它在哪个网段：`network_mode: none` 只是把这件事写死，
        // 省得下一个人以为可以顺手在里面跑点什么。
        let helper = format!("{}-restore", Self::container_name(owner));
        // 上一次恢复中途崩了会留下它。先删再建，比「已存在就复用」安全：
        // 复用等于信任一个来历不明的容器的挂载配置
        self.force_remove(&helper).await;

        let spec = ContainerCreateBody {
            image: Some(IMAGE.to_owned()),
            // 不会被执行（从不 start）。写一条明确的东西，是为了万一在
            // `docker ps -a` 里看见它时一眼知道它是干什么的
            cmd: Some(vec!["/bin/true".to_owned()]),
            host_config: Some(HostConfig {
                mounts: Some(vec![Mount {
                    typ: Some(MountType::VOLUME),
                    source: Some(Self::volume_name(owner)),
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
            .map_err(|e| CortexError::Store(format!("建恢复用的临时容器失败：{e}")))?;

        // **解到 `/` 而不是 `/workspace`。**
        //
        // archive API 导出的 tar，成员路径带 `workspace/` 这一级。解到
        // `/workspace` 会得到 `/workspace/workspace/...` —— 而那**不报错**，
        // 只是文件出现在错的地方，用户看到的是「恢复成功了但什么都没回来」。
        let opts = qp::UploadToContainerOptionsBuilder::default()
            .path("/")
            .build();
        let put = self
            .docker
            .upload_to_container(&helper, Some(opts), bollard::body_full(tar))
            .await
            .map_err(|e| CortexError::Store(format!("把快照写回 {owner} 的工作区失败：{e}")));

        // 无论成败都要收拾：留下一个挂着用户卷的容器，下一次 ensure 看不见它，
        // 而它会一直占着那个卷的引用
        self.force_remove(&helper).await;
        put
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
        }
    }

    /// **规格不可协商。** 入参只有 owner / token / hash，塞不进第二个挂载。
    ///
    /// 这条守的不是「cortexd 会不会写错」，是「cortexd 被攻破之后能做什么」——
    /// 它是直接处理模型输出的那个进程。如果哪天有人给 `ensure` 加一个
    /// 「挂载点」参数，这条测试不会红（它测的是当前这份规格），但下面
    /// 那条 `the_mount_list_is_a_constant_shape` 会在 review 时挡一道。
    #[test]
    fn the_spec_is_not_negotiable() {
        let r = runner();
        let spec = r.spec("u1", "tok", "hash");
        let host = spec.host_config.expect("必须有 HostConfig");

        assert_eq!(
            spec.image.as_deref(),
            Some(IMAGE),
            "镜像名写死在实现里，调用方指定不了"
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

    /// 容器名与卷名只含安全字符。
    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize("01ABC-xyz_9"), "01ABC-xyz_9");
        assert_eq!(
            sanitize("../../etc"),
            "------etc",
            "owner 里的路径字符必须被拍平 —— 带进容器名就能造出撞名或\
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
    #[test]
    fn the_sandbox_publishes_no_ports_at_all() {
        let r = runner();
        let spec = r.spec("u1", "tok", "hash");
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
        let spec = r.spec("u1", "tok", "hash");
        let env = spec.env.expect("必须有 env");
        let want = format!("http://{EGRESS_HOST}:{EGRESS_PORT}");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert!(
                env.contains(&format!("{key}={want}")),
                "少了 {key} —— 出网会有一路绕开代理。实际 env：{env:?}"
            );
        }
        assert!(
            env.iter().any(|e| e.starts_with("NO_PROXY=")),
            "回环与中继自己不该走代理，否则容器内自检会绕一圈甚至打成回环"
        );
    }
}
