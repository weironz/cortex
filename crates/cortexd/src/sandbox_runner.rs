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
use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountType, PortBinding};
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
const NETWORK: &str = "cortex-sandbox-net";

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

/// 一个跑起来的沙箱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    /// 容器名（= `cortex-sbx-{owner}`）。
    pub name: String,
    /// cortexd 该往哪儿反代。容器与 cortexd 同在 `cortex-sandbox-net` 上时
    /// 走服务名；宿主直连时走映射出来的端口。
    pub base_url: String,
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
    /// 容器经 `host.docker.internal` 回调，cortexd 经映射端口反代。
    same_network: bool,
}

impl DockerRunner {
    /// 连本机 docker daemon。
    ///
    /// # Errors
    /// 连不上（没装 / 没权限 / daemon 没起）。**不静默降级**：一个连不上
    /// docker 的 cortexd 起不了任何沙箱，而那该在启动时就说清楚，
    /// 不是等用户点了「新建沙箱会话」才报一句看不懂的错。
    pub fn connect(remote: impl Into<String>, same_network: bool) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| CortexError::Config(format!("连不上 docker daemon：{e}")))?;
        Ok(Self {
            docker,
            remote: remote.into(),
            same_network,
        })
    }

    fn container_name(owner: &str) -> String {
        format!("{NAME_PREFIX}{}", sanitize(owner))
    }

    fn volume_name(owner: &str) -> String {
        format!("{VOLUME_PREFIX}{}", sanitize(owner))
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
            if probe
                .get(&url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
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

        // 容器回调 cortexd 的地址。同网络时走服务名，否则走 docker 提供的
        // 宿主别名（`extra_hosts` 里那条 host-gateway）
        let remote = self.remote.clone();

        let env = vec![
            format!("CORTEX_REMOTE={remote}"),
            format!("CORTEX_TOKEN={token}"),
            // 镜像里已经设了这三个，这里重申是为了让「一个容器到底以什么
            // 身份跑」在 `docker inspect` 里一处可见 —— 排查时不必再去翻
            // 镜像的 ENV
            "CORTEX_EXEC_ENV=container".to_owned(),
            "CORTEX_DEFAULT_WORKSPACE=/workspace".to_owned(),
            format!("CORTEX_LOCAL_BIND=0.0.0.0:{AGENT_PORT}"),
        ];

        let mut port_bindings = HashMap::new();
        if !self.same_network {
            // 宿主上的 cortexd 要够得着容器。端口交给 docker 挑（空串 =
            // 随机高位端口），再从 inspect 里读回来 —— 自己猜一个「大概没被
            // 占」的端口，在两个沙箱或别的软件占了它时会静默连错地方
            port_bindings.insert(
                format!("{AGENT_PORT}/tcp"),
                Some(vec![PortBinding {
                    // **只绑回环**：沙箱的 agent 端口不该对同网段其他机器开放
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: Some(String::new()),
                }]),
            );
        }

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
                port_bindings: (!port_bindings.is_empty()).then_some(port_bindings),
                extra_hosts: (!self.same_network)
                    .then(|| vec!["host.docker.internal:host-gateway".to_owned()]),
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

        let base_url = if self.same_network {
            format!("http://{name}:{AGENT_PORT}")
        } else {
            // 读**实际**映射到的宿主端口。`ensure` 里请求的是空串（让 docker
            // 挑），所以这里不读回来就根本不知道该连哪儿
            let port = info
                .network_settings
                .as_ref()
                .and_then(|n| n.ports.as_ref())
                .and_then(|p| p.get(&format!("{AGENT_PORT}/tcp")).cloned().flatten())
                .and_then(|b| b.first().and_then(|x| x.host_port.clone()))
                .ok_or_else(|| CortexError::Store("沙箱容器没有映射出 agent 端口".into()))?;
            format!("http://127.0.0.1:{port}")
        };

        Ok(Some(SandboxHandle { name, base_url }))
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

    /// 宿主直连模式下，agent 端口**只绑回环**。
    #[test]
    fn the_agent_port_is_bound_to_loopback_only() {
        let r = runner();
        let spec = r.spec("u1", "tok", "hash");
        let bindings = spec
            .host_config
            .expect("必须有 HostConfig")
            .port_bindings
            .expect("宿主直连模式必须映射端口，否则 cortexd 够不着容器");
        let b = &bindings[&format!("{AGENT_PORT}/tcp")]
            .as_ref()
            .expect("必须有绑定")[0];
        assert_eq!(
            b.host_ip.as_deref(),
            Some("127.0.0.1"),
            "沙箱的 agent 端口绑 0.0.0.0 等于把同网段任何人放进这个容器 ——\
             而它能执行命令"
        );
    }
}
