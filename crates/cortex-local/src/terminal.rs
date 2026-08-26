//! 右栏「终端」页签背后的真终端 —— PTY 会话与它的 WS 端点。
//!
//! # 为什么是 WS + PTY，而不是复用 shell 工具
//!
//! shell 工具是「模型跑一条命令、拿一次输出」的形状：无 TTY、无交互、
//! 一问一答。用户要的终端是另一个东西 —— vim、top、交互式 REPL 都要一个
//! **真的 PTY**（Windows 上是 ConPTY）和一条双向的字节流。两者共享代码的
//! 只有「起子进程」这四个字，捏在一起反而把 shell 工具的沙箱语义搅浑。
//!
//! PTY 用 `portable-pty`（wezterm 系，Apache-2.0）：Windows 走 ConPTY、
//! unix 走 openpty，是「能搬就不写」里该搬的那种繁琐件。
//!
//! # 协议：二进制帧是字节流，文本帧是控制消息
//!
//! * 客户端 → agent：`Binary` = 敲进终端的原始字节；`Text` = JSON 控制消息
//!   （目前只有 `{"type":"resize","cols":N,"rows":N}`）。
//! * agent → 客户端：`Binary` = PTY 的原始输出。shell 退出时发一个带原因的
//!   Close 帧 —— 不发的话客户端只看到「断了」，分不清是网络还是 shell 自己
//!   `exit` 了。
//!
//! 不把控制消息塞进字节流里（比如约定一个转义序列）：终端字节流里**什么
//! 字节都可能出现**，任何带内约定都会被某个程序的输出撞上。WS 本来就有
//! 两种帧，正好各走各的。
//!
//! # 一个会话开几个终端：**不设上限**（2026-08-26 起）
//!
//! 此前是「每会话同时最多一个」，理由写的是「两个页签各拿一个 shell 对同一个
//! 工作区写东西，用户自己都说不清哪个是哪个」。界面上了多标签之后这条不成立
//! 了：每个 shell 在标签栏上有名字、有自己的 ×，谁是谁一目了然 —— 那时的
//! 「说不清」来自界面上根本没有第二个终端的位置，不是来自并发本身。
//!
//! 另一条理由是「重连风暴能堆出上百个 powershell」。它也不成立：客户端
//! **不自动重连** —— WS 一断就画一句原因加一个「重开一个 shell」的按钮，
//! 开第二个 shell 永远需要一次人点。
//!
//! 所以不再拦，只**记数并在数目反常时记一条 WARN**（见 [`Terminals`]）：
//! 上限拦得住失控，但也拦得住正当用法；日志两样都看得见，且不挡人。

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::state::LocalState;

/// 还没收到 resize 之前用的初始尺寸。客户端连上后第一次布局就会发真值，
/// 这里只要「不至于把首屏输出折得没法看」。
const INITIAL_SIZE: PtySize = PtySize {
    rows: 24,
    cols: 80,
    pixel_width: 0,
    pixel_height: 0,
};

/// 「此刻开着几个终端」的簿子。整个进程一份，挂在 [`LocalState`] 上。
///
/// 只**记数**，不持有 PTY 本身：PTY 的生命周期跟着那条 WS 连接走
/// （见 [`pump`]）。它不再拦任何人 —— 上限已经取消，见模块头。
///
/// 那为什么还留着它？因为「开了多少个」是唯一能让失控**看得见**的东西。
/// 一个每秒多一个 shell 的 bug，在没有这本簿子的世界里只表现为
/// 「这台机器怎么越来越慢」。
#[derive(Clone, Default)]
pub struct Terminals {
    live: Arc<Mutex<HashMap<String, usize>>>,
}

/// 多到该看一眼日志的数目。
///
/// 不是上限，是**记一条 WARN 的阈值**。挑 16 是因为「同时用着十几个 shell」
/// 已经罕见到值得留一行记录，而正常用法（两三个）永远碰不到它。
const NOISY: usize = 16;

impl Terminals {
    /// 记一个新终端。返回的 [`Slot`] 是 RAII 的：drop 即销账。
    ///
    /// **不提供显式的 release**，因为销账时机与「那条连接结束」必须严格
    /// 一致 —— 手动销账漏一条提前 return 的路，计数就永远偏高，
    /// 而那正是这本簿子唯一的用处。
    pub fn open(&self, session_id: &str) -> Slot {
        let mut n = 0;
        if let Ok(mut map) = self.live.lock() {
            let e = map.entry(session_id.to_string()).or_insert(0);
            *e += 1;
            n = *e;
        }
        tracing::debug!(session = %session_id, live = n, "开了一个终端");
        if n >= NOISY {
            tracing::warn!(
                session = %session_id,
                live = n,
                "这个会话同时开着很多终端 —— 上限已取消，这条只是让失控看得见"
            );
        }
        Slot {
            id: session_id.to_string(),
            live: Arc::clone(&self.live),
        }
    }

    /// 这个会话此刻开着几个。
    ///
    /// ⚠️ **只在测试里编译**：生产上这本簿子的出口是日志（开一个记一条
    /// debug、多到反常记一条 warn），没有第二个读者。留一个没人调的
    /// public 方法在这儿，下一个人会以为它是某处依赖的接口。
    #[cfg(test)]
    #[must_use]
    pub fn live_for(&self, session_id: &str) -> usize {
        self.live
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).copied())
            .unwrap_or(0)
    }
}

/// 一个终端的账，drop 即销。
pub struct Slot {
    id: String,
    live: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut map) = self.live.lock() {
            // 归零就把这条整个删掉：留着一堆 `= 0` 的条目，等于按会话数
            // 泄漏内存，而会话是能一直加的
            let mut left = 0;
            if let Some(n) = map.get_mut(&self.id) {
                *n = n.saturating_sub(1);
                left = *n;
                if *n == 0 {
                    map.remove(&self.id);
                }
            }
            // 与开的那条配对。**一直减不回 0 就是账漏了** —— 而账漏的
            // 唯一后果就是上面那条 WARN 早晚会在没人多开的时候响，
            // 到那时这两条日志是唯一能对上的东西
            tracing::debug!(session = %self.id, left, "关掉一个终端");
        }
    }
}

/// 起哪个 shell。Windows 是 powershell，其余按 `$SHELL`、回落 `/bin/bash`。
///
/// 不做成配置项：这个终端的定位是「顺手看一眼、跑两条命令」，要更讲究的
/// shell 的人自己会开真终端。
fn default_shell() -> String {
    shell_from(std::env::var("SHELL").ok().as_deref())
}

/// 上面那个的纯函数版本 —— **`$SHELL` 的值从参数进来**。
///
/// # 为什么要拆开
///
/// 「`$SHELL` 是空串时回落 `/bin/bash`」这一支在任何一台机器上都跑不到：
/// 开发机与 CI 的 `$SHELL` 都是设好的，而 edition 2024 里 `set_var` 是
/// unsafe，为一条测试去改进程环境不划算。拆出来之后两支都测得到。
///
/// 空串按没设处理 —— **空串顶掉默认值是这个仓库数过七次的形状**：
/// 一个 `SHELL=` 的环境（systemd 起的服务、某些容器）会让 agent 去执行
/// 一个名字为空的程序，报错是「找不到文件」，而没有一个字提到 `$SHELL`。
fn shell_from(env_shell: Option<&str>) -> String {
    if cfg!(windows) {
        return "powershell.exe".to_string();
    }
    env_shell
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| "/bin/bash".to_string(), str::to_owned)
}

/// shell 起在哪：绑了本机工作区就是那个目录，否则用户主目录。
///
/// 拿不到主目录时返回 `None` 让上层报错，而不是回落到进程 CWD ——
/// 桌面端拉起的 agent 的 CWD 是安装目录，在那里开 shell 只会让用户
/// 第一眼就迷路。
fn working_dir(binding: Option<String>) -> Option<PathBuf> {
    if let Some(dir) = binding {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// 一个活着的 PTY 会话：输入通道、resize 句柄、子进程。
///
/// 输出通道由 [`spawn_shell`] 单独交出去 —— 塞在这个结构体里的话，
/// `select!` 一边要 `&mut self` 读输出、另一边要 `&self` 写输入，
/// 借用当场打架。
pub struct PtySession {
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// 写入走独立线程（PTY 的写是阻塞的，不能在 async 任务里直接做）。
    /// 通道断了说明写线程已退，此时丢字节是对的 —— shell 都没了。
    input: std::sync::mpsc::Sender<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtySession {
    /// 把用户敲的字节送进 shell。失败静默：写线程退了 = shell 已死，
    /// 那件事由输出通道关闭来报，不必在这里再报一次。
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    /// 跟着客户端的窗口尺寸走。失败静默同上。
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// 子进程退了没有（非阻塞）。
    ///
    /// [`pump`] 靠它轮询「shell 自己 exit 了」：**不能只等输出通道关闭** ——
    /// ConPTY 上子进程退出并不必然让 master 的读端 EOF（EOF 要等
    /// `ClosePseudoConsole`，也就是 master 被 drop），于是 Windows 上
    /// 「用户敲了 exit」这件事从输出流里根本看不出来。实测：第一版只等
    /// EOF，测试在这儿挂了 60 秒。
    pub fn child_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// 收掉子进程并等它真的退了。返回「是否确认退出」。
    ///
    /// kill 之后必须 wait：不 wait 的话 unix 上留僵尸，Windows 上句柄
    /// 不回收。kill 过的进程 wait 会很快返回，所以这里的阻塞是有界的。
    pub fn shutdown(&mut self) -> bool {
        let _ = self.child.kill();
        self.child.wait().is_ok()
    }
}

impl Drop for PtySession {
    /// 兜底：不管哪条路提前 return，PTY 会话一被丢弃子进程就被收掉。
    /// 没有这条的话，「WS 断了但 powershell 还活着」不会有任何报错。
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 在 `cwd` 起一个 `program` 的 PTY 会话。
///
/// 返回会话本身 + 输出通道。输出通道关闭是 unix 上「shell 退了」的信号；
/// **Windows 上不是**（见 [`PtySession::child_exited`]），所以 [`pump`]
/// 两条都看：通道关闭，或轮询到子进程退出。
pub fn spawn_shell(
    program: &str,
    cwd: &std::path::Path,
    size: PtySize,
) -> anyhow::Result<(PtySession, tokio::sync::mpsc::Receiver<Vec<u8>>)> {
    let pty = native_pty_system();
    let pair = pty.openpty(size)?;
    let mut cmd = CommandBuilder::new(program);
    cmd.cwd(cwd);
    let child = pair.slave.spawn_command(cmd)?;
    // slave 端到子进程手里了，我们这份必须丢：留着的话子进程退出后
    // reader 永远等不到 EOF —— 「shell 已退出」那个信号就没了
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                // Ok(0) 是 EOF；Err 在 Windows 上是 ConPTY 关闭的常态报法
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // 接收端没了（连接断了），读也没意义了
                    }
                }
            }
        }
    });

    let mut writer = pair.master.take_writer()?;
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(bytes) = in_rx.recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    Ok((
        PtySession {
            master: pair.master,
            input: in_tx,
            child,
        },
        out_rx,
    ))
}

/// `GET /local/terminal/{session_id}` —— 升级成 WS，接上一个新起的 shell。
///
/// 认证走 [`crate::routes::require_auth`] 的**完整**那把（桌面端的
/// `dart:io` WebSocket 能带 Authorization 头）；远程接入那把够不到这条路
/// （见 `attach_allows`）—— 在别人机器上开任意 shell 是机器主人的权限。
pub async fn ws(
    State(st): State<LocalState>,
    Path(session_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    // 只在本机形态提供。容器里的这个进程理论上也能开 PTY，但那是另一个
    // 能力（进沙箱看现场），入口、鉴权、界面都还没有 —— 约束 2：
    // 没接好的能力不摆出来
    if st.engine.exec_env != cortex_agent::ExecEnvironment::LocalMachine {
        return (
            StatusCode::NOT_FOUND,
            "交互终端只在本机 agent 上提供，这个进程跑在容器里",
        )
            .into_response();
    }
    // 记一笔账。**不再拦**（上限已取消，见模块头）—— 它只让「开了多少个」
    // 在日志里看得见
    let slot = st.terminals.open(&session_id);
    let Some(cwd) = working_dir(st.engine.workspaces.get(&session_id)) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "拿不到主目录，不知道该在哪儿起 shell",
        )
            .into_response();
    };
    let shell = default_shell();
    match spawn_shell(&shell, &cwd, INITIAL_SIZE) {
        Ok((sess, output)) => upgrade.on_upgrade(move |sock| pump(sock, sess, output, slot)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("起不了 {shell}（cwd={}）：{e}", cwd.display()),
        )
            .into_response(),
    }
}

/// 双向搬运，直到 shell 退出或连接断开。结束时收掉子进程、归还席位。
async fn pump(
    mut sock: WebSocket,
    mut sess: PtySession,
    mut output: tokio::sync::mpsc::Receiver<Vec<u8>>,
    slot: Slot,
) {
    // 「shell 自己退了」在 Windows 上只能靠轮询（见 child_exited 的文档）。
    // 半秒一次：用户敲 exit 到页签显示「已退出」之间最多半秒，感知不到；
    // 而每条终端连接半秒一次 try_wait 的代价约等于零
    let mut child_poll = tokio::time::interval(Duration::from_millis(500));
    let exited = loop {
        tokio::select! {
            out = output.recv() => match out {
                Some(bytes) => {
                    if sock.send(Message::Binary(bytes.into())).await.is_err() {
                        break false;
                    }
                }
                // unix 上的「shell 退了」：master 读到 EOF
                None => break true,
            },
            _ = child_poll.tick() => {
                if sess.child_exited() {
                    // Windows 上的「shell 退了」。先把输出通道里还剩的
                    // 几帧发完 —— exit 前最后一屏不该被吞掉
                    while let Ok(bytes) = output.try_recv() {
                        if sock.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    break true;
                }
            }
            msg = sock.recv() => match msg {
                Some(Ok(Message::Binary(b))) => sess.write(b.to_vec()),
                Some(Ok(Message::Text(t))) => apply_control(&sess, &t),
                // Ping/Pong 由 axum 自动答
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break false,
            },
        }
    };
    if exited {
        // 带原因关闭 —— 客户端据此显示「shell 已退出」而不是「连接断了」：
        // 前者点重开就好，后者要去怀疑 agent 本身
        let _ = sock
            .send(Message::Close(Some(CloseFrame {
                code: axum::extract::ws::close_code::NORMAL,
                reason: "shell 已退出".into(),
            })))
            .await;
    }
    // 显式收掉（Drop 也会做，这里做是为了让「连接结束 = 子进程已死」
    // 在时序上确定，而不是等垃圾回收般的某个时刻）
    sess.shutdown();
    drop(slot);
}

/// 文本帧 = 控制消息。不认识的一律忽略 —— 协议以后加消息种类时，
/// 旧 agent 面对新客户端不该断连，顶多少一个功能。
fn apply_control(sess: &PtySession, raw: &str) {
    #[derive(serde::Deserialize)]
    struct Control {
        r#type: String,
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    }
    if let Ok(c) = serde_json::from_str::<Control>(raw)
        && c.r#type == "resize"
        && c.cols > 0
        && c.rows > 0
    {
        sess.resize(c.cols, c.rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **多开不再被拦，但要记得清清楚楚。**
    ///
    /// 上限 2026-08-26 取消了（理由见模块头：界面上了多标签，而客户端
    /// 从不自动重连）。取消一道闸的同时必须留下能看见失控的东西 ——
    /// 否则「这台机器怎么越来越慢」将没有任何线索。
    ///
    /// 所以这条钉两件事：多开放行，且计数**准**。计数不准的话这本簿子
    /// 就只剩误导 —— 一个永远偏高的数字比没有数字更糟。
    #[test]
    fn 同一会话开几个都放行_但账要记准() {
        let terminals = Terminals::default();

        let a = terminals.open("s1");
        let b = terminals.open("s1");
        let c = terminals.open("s2");
        assert_eq!(terminals.live_for("s1"), 2, "同会话第二个必须放行");
        assert_eq!(terminals.live_for("s2"), 1, "别的会话各记各的");

        drop(b);
        assert_eq!(
            terminals.live_for("s1"),
            1,
            "账是 RAII 的：连接结束（Slot drop）就该销掉，             漏一条提前 return 的路计数就永远偏高"
        );

        drop(a);
        drop(c);
        assert_eq!(terminals.live_for("s1"), 0);
        assert_eq!(
            terminals.live_for("s2"),
            0,
            "归零的会话要从簿子里整个删掉 —— 留着一堆 0 等于按会话数漏内存"
        );
    }

    /// shell 真的起得来、收得掉，且整个过程**有界**。
    ///
    /// 起的就是生产会用的那个 shell（Windows: powershell / unix: $SHELL），
    /// 所以这条测试同时验了「这台机器上默认 shell 存在」—— CI 的镜像
    /// 缺 bash 时在这里响，而不是用户点开页签时。
    ///
    /// 整段跑在带超时的线程里：第一版就是在这儿挂死 60 秒才发现
    /// ConPTY「子进程退了 ≠ 读端 EOF」（见 `child_exited` 的文档）。
    /// 挂死与失败必须是同一种红。
    #[test]
    fn spawn_then_shutdown_reaps_the_child() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cwd = tempfile::tempdir().expect("临时目录当 cwd");
            let (mut sess, mut output) = spawn_shell(&default_shell(), cwd.path(), INITIAL_SIZE)
                .expect("默认 shell 起不来 —— 终端页签在这台机器上就是坏的");

            let reaped = sess.shutdown();

            // ⚠️ EOF 之前必须 drop 掉 master（sess 里那份）：ConPTY 上
            // 读端 EOF 要等 ClosePseudoConsole，光杀子进程等不来 ——
            // pump 里对应的正是「结束时 sess 整个被 drop」这一步
            drop(sess);
            while output.blocking_recv().is_some() {}

            done_tx.send(reaped).ok();
        });

        let reaped = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("起停在 30 秒内没走完 —— kill/wait 或读线程的 EOF 挂死了");
        assert!(
            reaped,
            "shutdown 必须等到子进程真的退了：不等的话 unix 留僵尸、Windows 漏句柄"
        );
    }

    /// cwd 的判据：绑了工作区用工作区，没绑用主目录，主目录都没有就明说。
    #[test]
    fn working_dir_prefers_the_bound_workspace() {
        assert_eq!(
            working_dir(Some("D:/codes/proj".into())),
            Some(PathBuf::from("D:/codes/proj")),
            "绑了本机工作区的会话，shell 就该起在那个目录 —— 终端与文件页签看的是同一个地方"
        );
        // 这台机器（开发机 / CI）总有主目录；两个变量都没有的机器上这条会失败，
        // 而那种环境本来就起不了桌面端
        assert!(
            working_dir(None).is_some(),
            "没绑定时回落到主目录，而不是 agent 进程自己的 CWD（那是安装目录）"
        );
    }

    /// 控制消息：认识 resize，不认识的忽略、坏 JSON 忽略 —— 都不 panic。
    ///
    /// 这里能测的只有「不炸」：resize 的效果在 PTY 里，单元测试够不着。
    /// 但「一条恶意文本帧把整个 agent panic 掉」恰恰是这条路上最贵的失败。
    #[test]
    fn hostile_control_frames_do_not_panic() {
        let cwd = tempfile::tempdir().expect("临时目录");
        let (mut sess, _output) =
            spawn_shell(&default_shell(), cwd.path(), INITIAL_SIZE).expect("起 shell");

        for raw in [
            r#"{"type":"resize","cols":120,"rows":40}"#,
            r#"{"type":"resize","cols":0,"rows":0}"#, // 0 尺寸要拒：ConPTY 会报错
            r#"{"type":"future-thing","payload":true}"#,
            "not json at all",
            "",
        ] {
            apply_control(&sess, raw);
        }
        assert!(sess.shutdown(), "控制消息处理完 shell 应当还活着、还收得掉");
    }

    /// 空的 `$SHELL` 不算配置。
    #[test]
    fn default_shell_is_never_empty() {
        let shell = default_shell();
        assert!(
            !shell.trim().is_empty(),
            "shell 名是空的 —— 空串顶掉默认值，这个仓库第七次了"
        );
        if cfg!(windows) {
            assert_eq!(shell, "powershell.exe", "Windows 上按任务定义用 powershell");
        }
    }

    /// unix 上按 `$SHELL` 走，**空串与没设一样回落 `/bin/bash`**。
    ///
    /// 这一支在真机上跑不到（开发机与 CI 的 `$SHELL` 都设好了），所以它
    /// 只能在纯函数上测。跑不到不等于遇不到：`SHELL=` 的环境是真的存在
    /// （systemd 起的服务、某些精简容器），而那时的报错是「找不到文件」，
    /// 一个字都不提 `$SHELL`。
    #[test]
    fn unix_上按_shell_走_空串回落到_bash() {
        if cfg!(windows) {
            // Windows 上这个函数根本不看参数 —— 那也正是要钉的一件事：
            // 一个装了 WSL、`$SHELL` 指向 bash 的 Windows 机器，
            // 终端仍然该是 powershell
            assert_eq!(shell_from(Some("/bin/zsh")), "powershell.exe");
            return;
        }
        assert_eq!(shell_from(Some("/bin/zsh")), "/bin/zsh", "设了就用他的");
        assert_eq!(shell_from(Some("  ")), "/bin/bash", "全是空白 = 没设");
        assert_eq!(shell_from(Some("")), "/bin/bash", "空串 = 没设");
        assert_eq!(shell_from(None), "/bin/bash", "没这个变量");
        assert_eq!(
            shell_from(Some(" /usr/bin/fish ")),
            "/usr/bin/fish",
            "两头的空白要去掉 —— 留着的话执行的是一个带空格的路径，必定找不到"
        );
    }
}
