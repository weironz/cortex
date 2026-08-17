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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cortex_proto::dto::ChatEvent;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, broadcast};

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
    pub async fn attach(&self) -> (Vec<ChatEvent>, broadcast::Receiver<ChatEvent>) {
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

/// 一个会话最多有几条消息在排队等着跑（不含正在跑的那一条）。
///
/// # 为什么有个上限
///
/// 不设的话，一轮卡住 + 一个反复点发送的用户 = 内存无界，而且等它终于跑完
/// 之后会**一口气连跑十几轮** —— 每一轮都在改同一个工作区的文件，用户看着
/// 自己十分钟前那句话正在动今天的代码。
///
/// 4 足够覆盖真实的「手快连发两三条」，而第 5 条被明确拒掉比悄悄排上去好。
const MAX_QUEUED: usize = 4;

/// 一个会话在这个进程上的排队状态。
struct Slot {
    /// `GET /runs/{id}` 该挂哪一个：**正在跑的那个**，或者刚跑完还没被清掉的。
    ///
    /// 排在后面的那些**不在这里** —— 它们还没开始，重挂上去只会看到一片空白，
    /// 而用户想看的是此刻真的在动的那一轮。
    latest: Arc<Run>,
    /// 一次一轮的闸门，1 个许可。
    ///
    /// 用 `Semaphore` 而不是手写队列：**tokio 的 Semaphore 是 FIFO 公平的**，
    /// 于是「谁先到谁先跑」这条性质由它保证，我们不必自己维护一个
    /// `VecDeque` 加唤醒逻辑 —— 那正是最容易写出「偶尔插队」的地方。
    gate: Arc<Semaphore>,
    /// 正在等闸门的条数。**只用于报给用户看**（`ahead`）与限流。
    ///
    /// 不能从 `Semaphore` 问出来（它不暴露等待者数量），所以单独记一个。
    waiting: Arc<AtomicUsize>,
}

/// 这个进程上所有轮次。
#[derive(Clone, Default)]
pub struct Runs {
    inner: Arc<Mutex<HashMap<String, Slot>>>,
}

/// 这个会话排队的已经太多了。
#[derive(Debug)]
pub struct QueueFull {
    /// 当前排着几条，拼给用户看的那句话用。
    pub queued: usize,
}

/// 排队的凭据：**流已经可以给客户端了，但轮次还没开始。**
///
/// 分成两步是这条路的关键：`POST /chat` 必须**立刻**返回一条 SSE，
/// 哪怕前面还有一轮在跑。把 HTTP 请求按住等前面跑完的话，中间那些反代
/// （dev 的 nginx、生产的 traefik）会在读超时上先把它掐掉，而客户端看到的
/// 是「网络错误」——一次本该成功的排队变成一次失败。
pub struct Ticket {
    /// 这一轮自己的 `Run`。客户端马上挂上它，然后收 keepalive 直到轮到自己。
    pub run: Arc<Run>,
    /// 前面还有几轮。0 = 立刻就能跑。
    pub ahead: usize,
    session_id: String,
    gate: Arc<Semaphore>,
    waiting: Arc<AtomicUsize>,
    runs: Runs,
}

impl Ticket {
    /// 等到轮到自己，然后把这一轮登记成「当前在跑的」。
    ///
    /// 返回的许可**必须一直持有到这一轮彻底结束**（包括 episode 落库）——
    /// 提前 drop 等于放下一轮进来，而它读历史时会读不到上一轮的结果。
    pub async fn begin(&self) -> OwnedSemaphorePermit {
        let permit = Arc::clone(&self.gate)
            .acquire_owned()
            .await
            .expect("闸门不会被 close —— 没有任何地方调 Semaphore::close");
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        // 轮到自己了才成为「当前在跑的」，见 `Slot::latest` 的文档
        let mut map = self.runs.inner.lock().await;
        if let Some(slot) = map.get_mut(&self.session_id) {
            slot.latest = Arc::clone(&self.run);
        }
        permit
    }
}

impl Runs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 排一轮。**同一个会话已经在跑就排到它后面，而不是拒绝。**
    ///
    /// # 为什么是排队而不是拒绝
    ///
    /// 两轮同时跑一个会话，它们写同一个工作区的文件、往同一段历史里追加
    /// 消息，谁覆盖谁取决于时序 —— 所以**不能并跑**这条不变量没变。变的是
    /// 撞上的那一条怎么办：原先回 409，代价是**用户敲的那句话就此消失**，
    /// 而他往往正是因为看到「还在跑」才补了一句。
    ///
    /// 排队之后那句话不丢，只是晚一点被回答。OpenHands 在同一个问题上也
    /// 从「拒绝第二个客户端」改成了 FIFO 排队（见 references.md）。
    ///
    /// # Errors
    /// 排队的已经到 [`MAX_QUEUED`] 条。
    pub async fn enqueue(&self, session_id: &str) -> Result<Ticket, QueueFull> {
        let mut map = self.inner.lock().await;
        self.sweep(&mut map);
        let run = Arc::new(Run::new());
        // 这个会话第一次出现：开一把满的闸门，于是下面那段对「第一条」和
        // 「第五条」是同一段代码 —— 两条分支各算一次 `ahead` 的话，
        // 其中一条迟早算错，而算错的表现只是界面上多写/少写一个数字，
        // 不会有任何测试红
        if !map.contains_key(session_id) {
            map.insert(
                session_id.to_owned(),
                Slot {
                    latest: Arc::clone(&run),
                    gate: Arc::new(Semaphore::new(1)),
                    waiting: Arc::new(AtomicUsize::new(0)),
                },
            );
        }
        let slot = &map[session_id];

        // 前面还有几轮：正在跑的那一个（许可被占着）+ 已经在等的那些。
        //
        // 用 `available_permits` 而不是「latest 还在跑吗」：一轮的 `Done` 事件
        // 发出之后到那个 task 真的结束之间有一小段时间，许可还没放开。
        // 按 `latest` 判会在那一瞬间说「前面没人」，而 `begin` 实际还要等。
        let running = usize::from(slot.gate.available_permits() == 0);
        let waiting = slot.waiting.load(Ordering::SeqCst);
        if waiting >= MAX_QUEUED {
            return Err(QueueFull { queued: waiting });
        }
        slot.waiting.fetch_add(1, Ordering::SeqCst);
        Ok(Ticket {
            run,
            ahead: running + waiting,
            session_id: session_id.to_owned(),
            gate: Arc::clone(&slot.gate),
            waiting: Arc::clone(&slot.waiting),
            runs: self.clone(),
        })
    }

    /// 挂上一个还在跑（或刚跑完不久）的轮次。
    pub async fn attach(
        &self,
        session_id: &str,
    ) -> Option<(Vec<ChatEvent>, broadcast::Receiver<ChatEvent>, bool)> {
        let run = {
            let mut map = self.inner.lock().await;
            self.sweep(&mut map);
            Arc::clone(&map.get(session_id)?.latest)
        };
        let running = run.is_running().await;
        let (replay, rx) = run.attach().await;
        Some((replay, rx, running))
    }

    /// 现在有哪些在跑。
    pub async fn running(&self) -> Vec<RunSummary> {
        let map = self.inner.lock().await;
        let mut out = Vec::new();
        for (id, slot) in map.iter() {
            if slot.latest.is_running().await {
                out.push(RunSummary {
                    session_id: id.clone(),
                    started_at: slot.latest.started_at.to_rfc3339(),
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
    ///
    /// # 有人在排队时**一格都不能清**
    ///
    /// 清掉整个 slot 等于扔掉那把闸门，于是下一条消息会新建一把空的 ——
    /// 两轮就此并跑，而这正是这个模块存在的理由。所以先看闸门和等待数，
    /// 再看时间。
    fn sweep(&self, map: &mut HashMap<String, Slot>) {
        map.retain(|_, slot| {
            if slot.gate.available_permits() == 0 || slot.waiting.load(Ordering::SeqCst) > 0 {
                return true;
            }
            // `try_lock` 失败 = 正被别人用着，那它显然还活着
            slot.latest
                .finished_at
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

    /// 排一轮并等到它真的开跑。
    ///
    /// 返回的许可**必须接住**（`let (_run, _permit) = ...`）：drop 掉就等于告诉
    /// 闸门这一轮结束了，于是排在后面的立刻插进来 —— 一个 `let _ = ` 会让
    /// 「不许并跑」那几条测试无声地失去意义。
    async fn start(runs: &Runs, sid: &str) -> (Arc<Run>, OwnedSemaphorePermit) {
        let t = runs.enqueue(sid).await.expect("第一轮总排得进");
        let permit = t.begin().await;
        (Arc::clone(&t.run), permit)
    }

    /// **顺序要保住，相邻的 delta 要合并。**
    ///
    /// 合并是为了让缓冲的条数与工具调用数同阶（delta 一轮上万条）；
    /// 保序是因为「所有文字然后所有工具」重放出来是一个与当时完全不同的
    /// 过程 —— 用户会看到 agent 先写完再干活。
    #[tokio::test]
    async fn the_replay_keeps_order_and_folds_adjacent_deltas() {
        let runs = Runs::new();
        let (run, _permit) = start(&runs, "s1").await;
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
        let (run, _permit) = start(&runs, "s1").await;
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
        let (run, _permit) = start(&runs, "s1").await;
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

    /// 同一个会话仍然不许并跑 —— 但第二条是**排队**，不是被拒。
    ///
    /// 并跑那条不变量没变（两轮写同一个工作区、往同一段历史追加，谁覆盖谁
    /// 取决于时序）。变的是撞上的那一条怎么办：原先回 409，也就是用户敲的
    /// 那句话直接丢了。
    #[tokio::test]
    async fn a_second_turn_waits_instead_of_being_refused() {
        let runs = Runs::new();
        let (first, permit) = start(&runs, "s1").await;

        let second = runs.enqueue("s1").await.expect("第二条该排得进，不该被拒");
        assert_eq!(second.ahead, 1, "它前面有一轮在跑");

        // 前面还占着闸门，`begin` 必须等
        assert!(
            tokio::time::timeout(Duration::from_millis(50), second.begin())
                .await
                .is_err(),
            "第一轮还没结束，第二轮就开跑了 —— 这正是并跑"
        );

        // 第一轮结束（发 done 只是让 `is_running` 转假；真正放开闸门的是 drop 许可）
        RunSink::new(first)
            .send(ChatEvent::Done {
                episode_id: "e1".into(),
            })
            .await;
        drop(permit);

        let _permit2 = tokio::time::timeout(Duration::from_secs(1), second.begin())
            .await
            .expect("上一轮放开之后，排着的这一轮就该开跑");
    }

    /// **先到先跑。** 排队的顺序就是到达的顺序。
    ///
    /// 钉这条是因为它是「用 Semaphore 而不是手写队列」的全部理由：tokio 的
    /// 信号量本身 FIFO 公平。哪天有人为了「顺手」把它换成 `Notify` + 一个
    /// 布尔，这条会红 —— 那时的症状否则是「偶尔有一句话被插队，看起来像
    /// 模型答错了上一个问题」。
    #[tokio::test]
    async fn the_queue_runs_in_arrival_order() {
        let runs = Runs::new();
        let (_first, permit) = start(&runs, "s1").await;

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for i in 0..3u8 {
            let t = runs.enqueue("s1").await.expect("排得进");
            assert_eq!(
                t.ahead,
                usize::from(i) + 1,
                "第 {i} 条前面该有 {} 轮",
                i + 1
            );
            let order = Arc::clone(&order);
            handles.push(tokio::spawn(async move {
                let _p = t.begin().await;
                order.lock().await.push(i);
            }));
            // 让这个 task 真的跑到 `acquire` 上排队再放下一个 —— 否则三个
            // spawn 到达闸门的先后是调度顺序，测的就不是队列了
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        drop(permit);
        for h in handles {
            h.await.expect("排队的 task 不该 panic");
        }
        assert_eq!(*order.lock().await, vec![0, 1, 2], "跑的顺序必须是到达顺序");
    }

    /// 排到上限就明确拒掉，而不是继续收。
    ///
    /// 继续收的代价不是内存 —— 是等前面跑完之后**一口气连跑一串**十分钟前的
    /// 指令，每一条都在改同一份文件。
    #[tokio::test]
    async fn the_queue_is_bounded() {
        let runs = Runs::new();
        let (_first, _permit) = start(&runs, "s1").await;

        let mut tickets = Vec::new();
        for i in 0..MAX_QUEUED {
            tickets.push(
                runs.enqueue("s1")
                    .await
                    .unwrap_or_else(|_| panic!("第 {i} 条还在上限内，该排得进")),
            );
        }
        let full = runs.enqueue("s1").await;
        assert!(full.is_err(), "排满之后必须拒，否则就是无限攒指令");
        assert_eq!(
            full.err().expect("上面刚断言过是 Err").queued,
            MAX_QUEUED,
            "拒的时候要如实说排着几条 —— 那句话直接给用户看"
        );
    }

    /// 排队期间 `GET /runs/{id}` 挂到的是**正在跑的**那一轮，不是排在后面的。
    ///
    /// 反过来的话，一个回来重挂的用户会看到一片空白（排队的那轮还没发过任何
    /// 事件），而屏幕上明明有一轮在动。
    #[tokio::test]
    async fn attaching_while_queued_lands_on_the_running_turn() {
        let runs = Runs::new();
        let (first, _permit) = start(&runs, "s1").await;
        RunSink::new(first).send(delta("我是正在跑的那轮")).await;

        let _queued = runs.enqueue("s1").await.expect("排得进");

        let (replay, _, running) = runs.attach("s1").await.expect("挂得上");
        assert!(running, "挂到的该是还在跑的那一轮");
        assert!(
            matches!(&replay[..], [ChatEvent::Delta { text }] if text == "我是正在跑的那轮"),
            "挂到了排队那一轮（空的），而不是正在跑的：{replay:?}"
        );
    }

    /// 跑完的那条**留一会儿**，重挂还挂得上（只是 running=false）。
    ///
    /// 客户端在 `done` 到达前一瞬间断开是常事。立刻删掉的话它回来时
    /// 既挂不上、episode 也可能还没落库 —— 结果在两个来源的缝里消失几秒。
    #[tokio::test]
    async fn a_finished_run_is_still_attachable_for_a_while() {
        let runs = Runs::new();
        let (run, _permit) = start(&runs, "s1").await;
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
