//! 后台命令 —— 起一个不等它跑完的进程，之后再来看输出。
//!
//! # 为什么需要它
//!
//! `shell` 是同步单发：默认 120 秒、封顶 600 秒，超时就杀。于是
//! 「起一个 dev server 然后接着干别的」在这个产品里**做不到** ——
//! 模型要么把一轮挂死在那儿，要么在 10 分钟后拿到一条超时。
//!
//! 三家都有这个（Claude Code 的 `run_in_background` + `/tasks`、
//! Codex 的云端任务、Grok 的 async delegate），因为长命令在真实工作里
//! 到处都是：dev server、watch 模式的测试、一次大构建。
//!
//! # 为什么簿子在宿主，不在 `Turn`
//!
//! [`crate::Turn`] 刻意**无每轮状态**（同一个循环被三个宿主共用，
//! 那是它能被共用的原因）。而后台任务的寿命横跨好几轮 —— 它属于
//! 「会话」，而会话是宿主才有的概念。所以这里只提供簿子本身，
//! 由宿主持有一份。
//!
//! # 上限与清理
//!
//! 一个会话最多 [`MAX_TASKS`] 个后台任务，跑完的输出留在簿子里等人来取。
//! 没有上限的话，一个循环里的模型能起出几百个进程 —— 而它们都
//! `kill_on_drop(true)`，直到簿子被丢掉才收。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// 一个会话最多同时记多少个后台任务。
///
/// 8 个：足够 dev server + watch 测试 + 一两个长构建，又小到
/// 「模型跑飞了」时能当场看出来（第 9 个会被拒，且拒绝里写着为什么）。
pub const MAX_TASKS: usize = 8;

/// 每个任务最多留多少字节输出。
///
/// 一个 watch 模式的测试能刷出几百 MB。留头不留尾 —— **开头那部分才是
/// 有用的**：启动日志、第一条报错。尾部通常是同一行重复了一万次。
const MAX_OUTPUT: usize = 64 * 1024;

/// 一个后台任务此刻怎么样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// 还在跑。
    Running,
    /// 跑完了，退出码是这个。
    Exited(i32),
    /// 被 [`Tasks::kill`] 停掉的。
    Killed,
}

/// 簿子里的一条。
#[derive(Debug, Clone)]
pub struct TaskView {
    pub id: String,
    pub command: String,
    pub state: TaskState,
    /// 到目前为止的输出（stdout 与 stderr 合并，按到达顺序）。
    ///
    /// **合并而不是分开**：一条命令的报错与它前一行的进度输出是同一个
    /// 故事，分成两栏读的人要自己在脑子里按时间缝回去。
    pub output: String,
    /// 输出被截断过。界面与模型都要知道 —— 一段被悄悄截短的日志
    /// 会让人对着「怎么没有那一行」找半天。
    pub truncated: bool,
}

#[derive(Debug)]
struct Task {
    command: String,
    state: TaskState,
    output: String,
    truncated: bool,
    /// 杀它用。跑完之后置空。
    abort: Option<tokio::task::AbortHandle>,
}

/// 一个会话的后台任务簿。克隆廉价（内部 `Arc`）。
#[derive(Clone, Default)]
pub struct Tasks(Arc<Mutex<BTreeMap<String, Task>>>);

impl Tasks {
    /// 记下一个新任务，返回它的 id。
    ///
    /// # Errors
    /// 超过 [`MAX_TASKS`]。
    pub fn open(
        &self,
        id: &str,
        command: &str,
        abort: tokio::task::AbortHandle,
    ) -> Result<(), String> {
        let mut g = self.0.lock().map_err(|_| "任务簿锁坏了".to_string())?;
        // 只数还在跑的：跑完的那些留着是为了让人来取输出，
        // 不该占用「能同时跑几个」的名额
        let running = g.values().filter(|t| t.state == TaskState::Running).count();
        if running >= MAX_TASKS {
            return Err(format!(
                "这个会话已经有 {MAX_TASKS} 个后台任务在跑了。\
                 先用 background_output 看看它们，或者用 background_kill 停掉不要的。"
            ));
        }
        g.insert(
            id.to_string(),
            Task {
                command: command.to_string(),
                state: TaskState::Running,
                output: String::new(),
                truncated: false,
                abort: Some(abort),
            },
        );
        Ok(())
    }

    /// 追加一段输出。超过上限就丢掉新来的（留头不留尾）。
    pub fn append(&self, id: &str, chunk: &str) {
        let Ok(mut g) = self.0.lock() else { return };
        let Some(t) = g.get_mut(id) else { return };
        if t.output.len() >= MAX_OUTPUT {
            t.truncated = true;
            return;
        }
        let room = MAX_OUTPUT - t.output.len();
        if chunk.len() <= room {
            t.output.push_str(chunk);
        } else {
            // 按**字符边界**截，不按字节 —— 按字节会把一个汉字劈成半个，
            // 之后整段读起来是乱码
            let cut: String = chunk.chars().take(room).collect();
            t.output.push_str(&cut);
            t.truncated = true;
        }
    }

    /// 标记跑完了。
    pub fn finish(&self, id: &str, code: i32) {
        let Ok(mut g) = self.0.lock() else { return };
        if let Some(t) = g.get_mut(id) {
            t.state = TaskState::Exited(code);
            t.abort = None;
        }
    }

    /// 停掉一个。
    ///
    /// # Errors
    /// 没有这个 id、或者它已经跑完了。
    pub fn kill(&self, id: &str) -> Result<(), String> {
        let mut g = self.0.lock().map_err(|_| "任务簿锁坏了".to_string())?;
        let t = g
            .get_mut(id)
            .ok_or_else(|| format!("没有编号 {id} 的后台任务"))?;
        match t.state {
            TaskState::Running => {
                if let Some(h) = t.abort.take() {
                    h.abort();
                }
                t.state = TaskState::Killed;
                Ok(())
            }
            // 已经结束的不当错误：模型看到「跑完了」就知道不用再管，
            // 而一条报错会让它去想「是不是我记错了 id」
            _ => Ok(()),
        }
    }

    /// 看某一个。
    #[must_use]
    pub fn get(&self, id: &str) -> Option<TaskView> {
        let g = self.0.lock().ok()?;
        let t = g.get(id)?;
        Some(TaskView {
            id: id.to_string(),
            command: t.command.clone(),
            state: t.state.clone(),
            output: t.output.clone(),
            truncated: t.truncated,
        })
    }

    /// 列出全部（还在跑的排前面）。
    #[must_use]
    pub fn list(&self) -> Vec<TaskView> {
        let Ok(g) = self.0.lock() else {
            return Vec::new();
        };
        let mut out: Vec<TaskView> = g
            .iter()
            .map(|(id, t)| TaskView {
                id: id.clone(),
                command: t.command.clone(),
                state: t.state.clone(),
                // 列表里**不带输出** —— 八个任务的输出加起来能有半兆，
                // 而列表要回答的只是「有哪些、还在不在跑」
                output: String::new(),
                truncated: t.truncated,
            })
            .collect();
        out.sort_by_key(|t| (t.state != TaskState::Running, t.id.clone()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_abort() -> tokio::task::AbortHandle {
        // 一个立刻结束的任务，只为拿一个 AbortHandle
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { tokio::spawn(async {}).abort_handle() })
    }

    #[tokio::test]
    async fn 跑完的不占同时在跑的名额() {
        let tasks = Tasks::default();
        for i in 0..MAX_TASKS {
            let h = tokio::spawn(async {}).abort_handle();
            tasks
                .open(&format!("t{i}"), "sleep 1", h)
                .expect("前 8 个该收下");
        }
        let h = tokio::spawn(async {}).abort_handle();
        assert!(
            tasks.open("t9", "sleep 1", h).is_err(),
            "第 9 个要被拒 —— 没有上限的话，跑飞的循环能起出几百个进程"
        );

        tasks.finish("t0", 0);
        let h = tokio::spawn(async {}).abort_handle();
        assert!(
            tasks.open("t9", "sleep 1", h).is_ok(),
            "跑完的那些留着只是等人取输出，不该占用「能同时跑几个」的名额"
        );
    }

    #[test]
    fn 输出留头不留尾且按字符截断() {
        let tasks = Tasks::default();
        tasks.open("t", "echo", dummy_abort()).unwrap();
        tasks.append("t", "开头很重要");
        tasks.append("t", &"汉".repeat(MAX_OUTPUT));
        let v = tasks.get("t").unwrap();
        assert!(
            v.output.starts_with("开头很重要"),
            "启动日志与第一条报错在开头"
        );
        assert!(
            v.truncated,
            "截断过就要说出来 —— 悄悄截短会让人对着「怎么没有那一行」找半天"
        );
        assert!(
            v.output
                .chars()
                .all(|c| c == '汉' || "开头很重要".contains(c)),
            "按字节截会把一个汉字劈成半个，整段读起来是乱码"
        );
    }

    #[test]
    fn 停一个已经跑完的不算错() {
        let tasks = Tasks::default();
        tasks.open("t", "echo", dummy_abort()).unwrap();
        tasks.finish("t", 0);
        assert!(
            tasks.kill("t").is_ok(),
            "模型看到「跑完了」就知道不用再管；报错会让它去想「是不是我记错了 id」"
        );
        assert!(
            tasks.kill("nope").is_err(),
            "但不存在的 id 要报错 —— 那是它真的记错了"
        );
    }

    #[test]
    fn 列表不带输出() {
        let tasks = Tasks::default();
        tasks.open("t", "echo", dummy_abort()).unwrap();
        tasks.append("t", "一大段输出");
        assert!(
            tasks.list()[0].output.is_empty(),
            "八个任务的输出加起来能有半兆，而列表要回答的只是「有哪些、还在不在跑」"
        );
        assert_eq!(
            tasks.get("t").unwrap().output,
            "一大段输出",
            "单看那一个时才给"
        );
    }
}
