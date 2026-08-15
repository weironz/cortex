//! 正在跑的那些轮次。**让「关掉浏览器活还在干」这件事看得见。**
//!
//! # 之前缺的是哪一半
//!
//! 执行侧一直是对的：轮次跑在一个独立的 `tokio::spawn` 里，客户端断开只让
//! 转发那一层 `break`，轮次继续跑、episode 照常落库。
//!
//! **观测侧一格都没有。** 事件发往一个已经被丢弃的 channel，人回来只能等
//! episode 落库之后重新拉会话 —— 也就是说「派出去干活」这个能力存在，
//! 但用户看不见它在干，也接不回去。
//!
//! # 重放缓冲：保序 + 合并相邻 Delta
//!
//! 不是环形缓冲。丢掉最老的 delta 等于把回答开头截掉，而那正是用户回来时
//! 最想看的部分。
//!
//! 也不是「文本全攒成一坨、工具事件另存一列」：那样重放出来的顺序是
//! 「所有文字，然后所有工具」，而真实顺序是交替的 —— 用户看到的会是一段
//! 与当时完全不同的过程。
//!
//! 做法是**按原顺序存，只把相邻的 `Delta` 并成一条**。一轮里 delta 成千上万
//! 但工具调用只有几个，于是缓冲的条数是「工具调用数 + 1」量级，
//! 而内容体量就是这一轮回答本身 —— 它本来就要全部发给客户端。
//!
//! # `Confirm` 不进缓冲
//!
//! 那是一次性凭据，重放一个已经被答复过的 token 只会让客户端弹一个点了
//! 就 404 的框。补拉确认本来就有自己的路（`GET /confirmations`），
//! 客户端重挂时走那条 —— 两条路各司其职，比在这里复制一份状态可靠。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cortex_proto::dto::ChatEvent;
use tokio::sync::{Mutex, broadcast};

/// 跑完之后这条记录还留多久。
///
/// # 为什么不是「跑完就删」
///
/// 客户端在 `done` 到达前一瞬间断开是常事（关标签页、切网络）。立刻删掉
/// 的话它回来时既 attach 不上、episode 也可能还没落库 —— 于是那一轮的
/// 结果在两个来源之间的缝里消失了几秒，表现为「刚才那条回答呢」。
///
/// 留一会儿，重挂就能立刻拿到完整的重放加一条 `done`。
const KEEP_FINISHED: Duration = Duration::from_secs(300);

/// 广播通道的容量。
///
/// 满了之后**慢的订阅者会丢事件**（`RecvError::Lagged`），而不是把发送方
/// 堵住 —— 这正是要的：一个卡住的浏览器标签页不该让 agent 停下来。
/// 丢了的那一段靠客户端重新 attach 补（重放缓冲是完整的）。
const BROADCAST_CAP: usize = 256;

/// 一轮的实时状态。
pub struct Run {
    /// 开始时间。界面拿它显示「已经跑了 3 分钟」。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 跑完的时间。`None` = 还在跑。
    finished_at: Mutex<Option<Instant>>,
    /// 保序的重放缓冲，见模块文档。
    replay: Mutex<Vec<ChatEvent>>,
    tx: broadcast::Sender<ChatEvent>,
}

impl Run {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        Self {
            started_at: chrono::Utc::now(),
            finished_at: Mutex::new(None),
            replay: Mutex::new(Vec::new()),
            tx,
        }
    }

    /// 记一条事件并广播出去。
    ///
    /// **两件事在同一把锁下做**：先写缓冲再发广播，中间不放锁。
    /// 放开的话，一个正在 attach 的订阅者可能拿到一份还没写进这条事件的
    /// 快照，同时又错过这次广播 —— 那条事件就凭空消失了。
    async fn record(&self, ev: &ChatEvent) {
        // 确认是一次性凭据，不进缓冲（见模块文档），但**要广播** ——
        // 正连着的客户端就靠它弹确认框
        if !matches!(ev, ChatEvent::Confirm { .. }) {
            let mut buf = self.replay.lock().await;
            match (buf.last_mut(), ev) {
                // 相邻的 Delta 并成一条
                (Some(ChatEvent::Delta { text: acc }), ChatEvent::Delta { text }) => {
                    acc.push_str(text);
                }
                _ => buf.push(ev.clone()),
            }
        }
        // 没有订阅者时 `send` 回 Err，那是正常的（用户关掉了页面）
        self.tx.send(ev.clone()).ok();
    }

    /// 快照 + 订阅，**在同一把锁下**。见 [`Self::record`]。
    async fn attach(&self) -> (Vec<ChatEvent>, broadcast::Receiver<ChatEvent>) {
        let buf = self.replay.lock().await;
        let rx = self.tx.subscribe();
        (buf.clone(), rx)
    }

    async fn finish(&self) {
        *self.finished_at.lock().await = Some(Instant::now());
    }

    pub async fn is_running(&self) -> bool {
        self.finished_at.lock().await.is_none()
    }
}

/// 这个进程上所有轮次。
#[derive(Clone, Default)]
pub struct Runs {
    inner: Arc<Mutex<HashMap<String, Arc<Run>>>>,
}

/// 一条已经在跑了。
#[derive(Debug)]
pub struct AlreadyRunning;

impl Runs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 开一轮。**同一个会话已经在跑就拒绝。**
    ///
    /// # 为什么拒绝而不是并跑
    ///
    /// 两轮同时跑一个会话，它们写同一个工作区的文件、往同一段历史里追加
    /// 消息，谁覆盖谁取决于时序。客户端自己有「一次一轮」的守卫，所以走到
    /// 这里的必然是**另一个设备或另一个标签页** —— 那个人不知道这边正在跑，
    /// 而一句「这个会话正在跑，等它跑完」比一堆交错的文件改动好得多。
    pub async fn begin(&self, session_id: &str) -> Result<Arc<Run>, AlreadyRunning> {
        let mut map = self.inner.lock().await;
        self.sweep(&mut map);
        if let Some(existing) = map.get(session_id)
            && existing.is_running().await
        {
            return Err(AlreadyRunning);
        }
        let run = Arc::new(Run::new());
        map.insert(session_id.to_owned(), Arc::clone(&run));
        Ok(run)
    }

    /// 挂上一个还在跑（或刚跑完不久）的轮次。
    pub async fn attach(
        &self,
        session_id: &str,
    ) -> Option<(Vec<ChatEvent>, broadcast::Receiver<ChatEvent>, bool)> {
        let run = {
            let mut map = self.inner.lock().await;
            self.sweep(&mut map);
            Arc::clone(map.get(session_id)?)
        };
        let running = run.is_running().await;
        let (replay, rx) = run.attach().await;
        Some((replay, rx, running))
    }

    /// 现在有哪些在跑。
    pub async fn running(&self) -> Vec<RunSummary> {
        let map = self.inner.lock().await;
        let mut out = Vec::new();
        for (id, run) in map.iter() {
            if run.is_running().await {
                out.push(RunSummary {
                    session_id: id.clone(),
                    started_at: run.started_at.to_rfc3339(),
                });
            }
        }
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }

    /// 清掉留够了时间的已完成记录。
    ///
    /// 顺手做，不开后台任务：这张表只在有人开轮次 / 重挂 / 列表时被碰，
    /// 而一个专门的定时器意味着多一个要管生命周期的东西，换来的只是
    /// 「没人用的时候内存早几分钟释放」。
    fn sweep(&self, map: &mut HashMap<String, Arc<Run>>) {
        map.retain(|_, run| {
            // `try_lock` 失败 = 正被别人用着，那它显然还活着
            run.finished_at
                .try_lock()
                .is_ok_and(|f| f.is_none_or(|t| t.elapsed() < KEEP_FINISHED))
        });
    }
}

/// `GET /runs` 里的一条。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunSummary {
    pub session_id: String,
    /// RFC3339。界面拿它算「已经跑了多久」。
    pub started_at: String,
}

/// 把一轮的事件同时送进重放缓冲与广播。
///
/// `Engine::chat` 用它替代原先那个裸 `mpsc::Sender` —— 于是**发起的那条
/// 连接与后来重挂的连接走的是同一份数据**，不存在「实时那条看得见、
/// 重挂那条看不见」的字段。
pub struct RunSink {
    run: Arc<Run>,
}

impl RunSink {
    #[must_use]
    pub fn new(run: Arc<Run>) -> Self {
        Self { run }
    }

    pub async fn send(&self, ev: ChatEvent) {
        let terminal = matches!(ev, ChatEvent::Done { .. } | ChatEvent::Error { .. });
        self.run.record(&ev).await;
        if terminal {
            self.run.finish().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(s: &str) -> ChatEvent {
        ChatEvent::Delta { text: s.into() }
    }

    fn tool(name: &str) -> ChatEvent {
        ChatEvent::Tool {
            name: name.into(),
            summary: String::new(),
            path: None,
            diff: None,
        }
    }

    /// **顺序要保住，相邻的 delta 要合并。**
    ///
    /// 合并是为了让缓冲的条数与工具调用数同阶（delta 一轮上万条）；
    /// 保序是因为「所有文字然后所有工具」重放出来是一个与当时完全不同的
    /// 过程 —— 用户会看到 agent 先写完再干活。
    #[tokio::test]
    async fn the_replay_keeps_order_and_folds_adjacent_deltas() {
        let runs = Runs::new();
        let run = runs.begin("s1").await.expect("第一轮");
        let sink = RunSink::new(Arc::clone(&run));

        sink.send(delta("你")).await;
        sink.send(delta("好")).await;
        sink.send(tool("read_file")).await;
        sink.send(delta("看")).await;
        sink.send(delta("完了")).await;

        let (replay, _rx, running) = runs.attach("s1").await.expect("挂得上");
        assert!(running);
        assert_eq!(replay.len(), 3, "两段文字 + 一个工具，实际：{replay:?}");
        assert!(matches!(&replay[0], ChatEvent::Delta { text } if text == "你好"));
        assert!(matches!(&replay[1], ChatEvent::Tool { name, .. } if name == "read_file"));
        assert!(matches!(&replay[2], ChatEvent::Delta { text } if text == "看完了"));
    }

    /// 重挂之后**接着**收得到新事件，而且不重不漏。
    ///
    /// 这条钉的是 `attach` 里「快照与订阅在同一把锁下」那一句：分开做的话
    /// 中间那几条事件既不在快照里、也不在订阅里，凭空消失。
    #[tokio::test]
    async fn attaching_then_receives_the_events_that_follow() {
        let runs = Runs::new();
        let run = runs.begin("s1").await.expect("第一轮");
        let sink = RunSink::new(Arc::clone(&run));

        sink.send(delta("前半")).await;
        let (replay, mut rx, _) = runs.attach("s1").await.expect("挂得上");
        sink.send(delta("后半")).await;

        assert_eq!(replay.len(), 1);
        let next = rx.recv().await.expect("挂上之后的事件要收得到");
        assert!(matches!(next, ChatEvent::Delta { text } if text == "后半"));
    }

    /// 确认事件**广播但不进缓冲**。
    ///
    /// 重放一个已经被答复过的 token，客户端会弹一个点了就 404 的框。
    #[tokio::test]
    async fn a_confirm_is_broadcast_but_never_replayed() {
        let runs = Runs::new();
        let run = runs.begin("s1").await.expect("第一轮");
        let sink = RunSink::new(Arc::clone(&run));
        let (_, mut rx, _) = runs.attach("s1").await.expect("挂得上");

        sink.send(ChatEvent::Confirm {
            token: "t".into(),
            tool: "shell".into(),
            risk: "execute".into(),
            preview: "rm -rf /".into(),
            timeout_secs: 60,
            scope: None,
            diff: None,
        })
        .await;

        assert!(
            matches!(rx.recv().await, Ok(ChatEvent::Confirm { .. })),
            "正连着的客户端要收到它 —— 确认框就靠这条"
        );
        let (replay, _, _) = runs.attach("s1").await.expect("挂得上");
        assert!(
            replay.is_empty(),
            "但它不该进重放缓冲，否则重挂会弹一个已经作废的确认框：{replay:?}"
        );
    }

    /// 同一个会话不许并跑。
    ///
    /// 走到这一步的必然是另一个设备 / 另一个标签页 —— 客户端自己有守卫。
    /// 让它们并跑的话，两轮写同一个工作区、往同一段历史追加，谁覆盖谁
    /// 取决于时序。
    #[tokio::test]
    async fn a_second_turn_on_the_same_session_is_refused() {
        let runs = Runs::new();
        let run = runs.begin("s1").await.expect("第一轮");
        assert!(runs.begin("s1").await.is_err(), "第二轮必须被拒");

        // 跑完之后可以再开
        RunSink::new(run)
            .send(ChatEvent::Done {
                episode_id: "e1".into(),
            })
            .await;
        assert!(runs.begin("s1").await.is_ok(), "上一轮跑完了就该能再开");
    }

    /// 跑完的那条**留一会儿**，重挂还挂得上（只是 running=false）。
    ///
    /// 客户端在 `done` 到达前一瞬间断开是常事。立刻删掉的话它回来时
    /// 既挂不上、episode 也可能还没落库 —— 结果在两个来源的缝里消失几秒。
    #[tokio::test]
    async fn a_finished_run_is_still_attachable_for_a_while() {
        let runs = Runs::new();
        let run = runs.begin("s1").await.expect("第一轮");
        let sink = RunSink::new(run);
        sink.send(delta("答完了")).await;
        sink.send(ChatEvent::Done {
            episode_id: "e1".into(),
        })
        .await;

        let (replay, _, running) = runs.attach("s1").await.expect("跑完之后仍该挂得上");
        assert!(!running, "但要如实说它已经跑完了");
        assert_eq!(replay.len(), 2, "文字与 done 都在：{replay:?}");
        assert!(
            runs.running().await.is_empty(),
            "「正在跑」的列表里不该有它"
        );
    }
}
