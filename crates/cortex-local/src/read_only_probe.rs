//! 有硬等待预算的只读单飞探针。超时不会取消 OS 工作，也不会释放单飞槽位。
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
const BUDGET: Duration = Duration::from_millis(80);
const DISPLAY_TTL: Duration = Duration::from_millis(500);

#[derive(Clone)]
enum Reply<T> {
    Pending,
    Done(Option<T>),
}
struct Flight<T: Clone> {
    reply: watch::Sender<Reply<T>>,
}
struct State<T: Clone> {
    flight: Option<Arc<Flight<T>>>,
    last: Option<(Instant, T)>,
    generation: u64,
}
pub(crate) struct ReadOnlyProbe<T: Clone> {
    state: Mutex<State<T>>,
    #[cfg(test)]
    starts: std::sync::atomic::AtomicUsize,
}
impl<T: Clone + Send + Sync + 'static> ReadOnlyProbe<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                flight: None,
                last: None,
                generation: 0,
            }),
            #[cfg(test)]
            starts: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    #[cfg(test)]
    pub(crate) fn starts(&self) -> usize {
        self.starts.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// 已由真实写入路径确认的状态更新；旧 probe 不能把新 fence 覆盖成 Ready。
    pub(crate) fn update(&self, value: T) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.last = Some((Instant::now(), value));
        }
    }
    #[cfg(test)]
    pub(crate) fn clear_display(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.last = None;
        }
    }
    pub(crate) async fn read(
        self: &Arc<Self>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (flight, start, generation) = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => return None,
            };
            if let Some((at, value)) = &state.last
                && at.elapsed() < DISPLAY_TTL
            {
                return Some(value.clone());
            }
            if let Some(flight) = &state.flight {
                (Arc::clone(flight), false, state.generation)
            } else {
                let (reply, _) = watch::channel(Reply::Pending);
                let flight = Arc::new(Flight { reply });
                state.flight = Some(Arc::clone(&flight));
                (flight, true, state.generation)
            }
        };
        if start {
            #[cfg(test)]
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let owner = Arc::clone(self);
            let completed = Arc::clone(&flight);
            let spawned = std::thread::Builder::new()
                .name("computer-readiness-blocking".into())
                .spawn(move || {
                    // work 必须在返回前关闭自己的存储/子 runtime；槽位不会在 Drop 等待前释放。
                    let result = work();
                    if let Ok(mut state) = owner.state.lock() {
                        let result = if state.generation == generation {
                            state.last = Some((Instant::now(), result.clone()));
                            Some(result)
                        } else {
                            state
                                .last
                                .as_ref()
                                .filter(|(at, _)| at.elapsed() < DISPLAY_TTL)
                                .map(|(_, value)| value.clone())
                        };
                        completed.reply.send_replace(Reply::Done(result));
                        state.flight = None;
                    }
                });
            if spawned.is_err() {
                flight.reply.send_replace(Reply::Done(None));
                if let Ok(mut state) = self.state.lock() {
                    state.flight = None;
                }
            }
        }
        let mut reply = flight.reply.subscribe();
        let wait = async move {
            loop {
                let value = reply.borrow().clone();
                if let Reply::Done(value) = value {
                    return value;
                }
                if reply.changed().await.is_err() {
                    return None;
                }
            }
        };
        tokio::time::timeout(BUDGET, wait).await.ok().flatten()
    }
}
