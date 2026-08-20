//! 断网期间的写入队列。
//!
//! 连不上 cortexd 时，agent 循环照常跑（工具、shell、LLM 全在本地），
//! 只是这一轮的 episode 写不回记忆库。排进这里，联网后按序重放。
//!
//! # 形态：append-only JSONL + 一个高水位
//!
//! `outbox.jsonl` 每行一个完整的 `POST /episodes` 请求体，只追加。
//! `outbox.mark` 存最后一条**成功刷出**的行号，重放从 mark+1 开始。
//!
//! 高水位而不是「刷出后删掉那一行」：删行要重写整个文件，而重写过程中
//! 崩溃会同时丢掉已刷和未刷的。追加 + 高水位的最坏情况是**重放一遍已经
//! 刷过的**，而那是幂等的（见 `cortex_proto::episodes` 的幂等约定）。
//!
//! # 一条不能松的规矩：中间损坏必须响亮失败
//!
//! 崩溃时最后一行可能只写了一半。那一行丢掉是对的 —— 它本来就没写完，
//! 而调用方还没收到「已入队」的确认。
//!
//! 但**中间**某一行解析不出来是另一回事：那说明文件被破坏了，而跳过它
//! 等于静默丢掉一轮对话 —— 正是这个队列存在的理由。所以中间损坏一律
//! 报错，让人去看，而不是自作主张地跳过去。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cortex_core::{CortexError, Result};
use cortex_proto::episodes::NewEpisodeRequest;

const QUEUE_FILE: &str = "outbox.jsonl";
const MARK_FILE: &str = "outbox.mark";

/// 一条待重放的记录：行号 + 请求体。
#[derive(Debug)]
pub struct Queued {
    /// 1 起的行号，就是高水位用的那个序
    pub seq: u64,
    pub request: NewEpisodeRequest,
}

/// 队列。**不是** `Send + Sync` 的共享句柄 —— 它每次操作都重开文件，
/// 因为一个跨 await 持有的文件句柄会在 Windows 上把文件锁住，
/// 而那正是「重放跑着的时候写不进新的一轮」。
#[derive(Debug, Clone)]
pub struct Outbox {
    queue: PathBuf,
    mark: PathBuf,
}

impl Outbox {
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            queue: dir.join(QUEUE_FILE),
            mark: dir.join(MARK_FILE),
        }
    }

    /// 排进队尾。
    ///
    /// 一行一次 `write_all` 再 `flush`：**不做**逐字段流式写入。
    /// 一次系统调用写完整行，把「半行」的窗口压到最小 ——
    /// 压不到零（内核可以只落一半），所以读那侧仍要能处理尾部半行。
    ///
    /// # Errors
    /// 序列化失败、文件打不开、写失败。任何一种都必须让调用方知道 ——
    /// 「排队失败」被吞掉的话，用户以为这一轮存下了，其实没有。
    pub fn push(&self, req: &NewEpisodeRequest) -> Result<u64> {
        let mut line = serde_json::to_string(req)
            .map_err(|e| CortexError::Invalid(format!("序列化待重放的 episode 失败：{e}")))?;
        line.push('\n');

        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.queue)
            .map_err(|e| self.io_err("打开", e))?;
        f.write_all(line.as_bytes())
            .map_err(|e| self.io_err("写入", e))?;
        f.flush().map_err(|e| self.io_err("flush", e))?;

        let seq = self.count_lines()?;
        tracing::info!(
            seq,
            episode = %req.id,
            "cortexd 连不上，这一轮已排进本地队列"
        );
        Ok(seq)
    }

    /// 还没刷出去的那些。
    ///
    /// # Errors
    /// 文件读不了，或**中间**某一行解析失败（见模块注释：那不是可以跳过的）。
    pub fn pending(&self) -> Result<Vec<Queued>> {
        if !self.queue.exists() {
            return Ok(Vec::new());
        }
        let mark = self.read_mark();
        let f = File::open(&self.queue).map_err(|e| self.io_err("打开", e))?;
        let lines: Vec<std::io::Result<String>> = BufReader::new(f).lines().collect();
        let total = lines.len();

        let mut out = Vec::new();
        for (i, line) in lines.into_iter().enumerate() {
            let seq = i as u64 + 1;
            let line = line.map_err(|e| self.io_err("读", e))?;
            if seq <= mark {
                continue;
            }
            match serde_json::from_str::<NewEpisodeRequest>(&line) {
                Ok(request) => out.push(Queued { seq, request }),
                // 尾部半行：崩溃时写了一半，调用方也没收到「已入队」的确认
                Err(e) if i + 1 == total => {
                    tracing::warn!(
                        seq,
                        error = %e,
                        "队列最后一行不完整（崩溃时写了一半），丢弃"
                    );
                }
                // 中间损坏：跳过等于静默丢一轮对话
                Err(e) => {
                    return Err(CortexError::Invalid(format!(
                        "本地队列 {} 的第 {seq} 行解析失败：{e}。\n\
                         这一行在文件中间，不是崩溃留下的半行 —— 跳过它等于\
                         静默丢掉一轮对话。请人工检查该文件后再启动。",
                        self.queue.display()
                    )));
                }
            }
        }
        Ok(out)
    }

    /// 记下「到 `seq` 为止都刷出去了」。
    ///
    /// 先写临时文件再 rename：rename 在同一文件系统内是原子的，
    /// 而直接覆写有一个「旧值没了新值还没落」的窗口 —— 崩在那里
    /// 会让高水位归零，于是整个队列重放一遍。幂等兜得住，但那是
    /// 一次可以避免的全量重放。
    pub fn commit(&self, seq: u64) -> Result<()> {
        let tmp = self.mark.with_extension("mark.tmp");
        std::fs::write(&tmp, seq.to_string()).map_err(|e| self.io_err("写高水位", e))?;
        std::fs::rename(&tmp, &self.mark).map_err(|e| self.io_err("提交高水位", e))?;

        // 全部刷完就把文件清掉，否则它会无界增长。
        // 截断而不是删除：删掉之后下次 push 要重新创建，而创建失败
        // （目录被删了、权限变了）会在**排队那一刻**才暴露 —— 也就是
        // 断网的时候，最不该再出一个错的时候
        if seq >= self.count_lines()? {
            OpenOptions::new()
                .write(true)
                .open(&self.queue)
                .and_then(|f| {
                    f.set_len(0)?;
                    let mut f = f;
                    f.seek(SeekFrom::Start(0)).map(|_| ())
                })
                .map_err(|e| self.io_err("截断", e))?;
            std::fs::write(&self.mark, "0").map_err(|e| self.io_err("重置高水位", e))?;
            tracing::info!("本地队列已全部刷出，文件已截断");
        }
        Ok(())
    }

    /// 队列里还有多少条没刷出去。给 `/health` 与界面用。
    pub fn backlog(&self) -> u64 {
        self.count_lines()
            .unwrap_or(0)
            .saturating_sub(self.read_mark())
    }

    fn count_lines(&self) -> Result<u64> {
        if !self.queue.exists() {
            return Ok(0);
        }
        let f = File::open(&self.queue).map_err(|e| self.io_err("打开", e))?;
        Ok(BufReader::new(f).lines().count() as u64)
    }

    /// 高水位读不出来一律当 0 —— 宁可全量重放（幂等，代价是几次多余的
    /// HTTP），也不能猜一个偏大的值，那会**永久跳过**中间那几条。
    fn read_mark(&self) -> u64 {
        std::fs::read_to_string(&self.mark)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn io_err(&self, what: &str, e: std::io::Error) -> CortexError {
        CortexError::Config(format!("{what}本地队列 {} 失败：{e}", self.queue.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_proto::episodes::NewEpisodeRequest;

    fn req(id: &str, text: &str) -> NewEpisodeRequest {
        NewEpisodeRequest {
            id: id.to_string(),
            session_id: "s".into(),
            role: "user".into(),
            text: text.into(),
            occurred_at: None,
            attachments: Vec::new(),
            retrieve: false,
            anchor_episode_id: None,
            tool_calls: Vec::new(),
            models: Vec::new(),
        }
    }

    /// 基本往返：排三条，读出三条，顺序不变。
    #[test]
    fn three_pushes_come_back_in_order() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for (i, t) in ["一", "二", "三"].iter().enumerate() {
            ob.push(&req(&format!("id-{i}"), t)).expect("排队应当成功");
        }
        let got = ob.pending().expect("读队列应当成功");
        let texts: Vec<_> = got.iter().map(|q| q.request.text.as_str()).collect();
        assert_eq!(
            texts,
            ["一", "二", "三"],
            "顺序必须保持 —— 乱序重放会让对话在远端的时间线上错位"
        );
        assert_eq!(got[0].seq, 1, "行号从 1 起");
    }

    /// 刷出去一部分之后，只剩下没刷的。
    #[test]
    fn committing_a_prefix_leaves_only_the_rest() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for i in 0..3 {
            ob.push(&req(&format!("id-{i}"), "x")).expect("排队");
        }
        ob.commit(2).expect("提交高水位");
        let got = ob.pending().expect("读队列");
        assert_eq!(got.len(), 1, "刷出两条后应当只剩一条");
        assert_eq!(got[0].seq, 3, "剩下的是第三条");
        assert_eq!(ob.backlog(), 1);
    }

    /// **尾部半行**：崩溃时最后一行只写了一半 —— 丢弃它，前面的照常重放。
    ///
    /// 那一行本来就没写完，调用方也没收到「已入队」的确认。
    #[test]
    fn a_truncated_last_line_is_dropped_and_the_rest_survives() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for i in 0..3 {
            ob.push(&req(&format!("id-{i}"), "x")).expect("排队");
        }
        // 砍掉尾部若干字节，制造一行残缺的 JSON
        let path = dir.path().join(QUEUE_FILE);
        let len = std::fs::metadata(&path).expect("元数据").len();
        let f = OpenOptions::new().write(true).open(&path).expect("打开");
        f.set_len(len - 20).expect("截断");
        drop(f);

        let got = ob.pending().expect("尾部半行不该让整个队列读失败");
        assert_eq!(got.len(), 2, "前两条必须活下来 —— 只有写了一半的那条才该丢");
    }

    /// **中间损坏**：必须响亮失败，绝不跳过。
    ///
    /// 跳过一条中间的坏行 = 静默丢掉一轮对话，而这个队列存在的全部理由
    /// 就是不丢。这条测试是这个模块最要紧的一条。
    #[test]
    fn a_corrupt_middle_line_fails_loudly_instead_of_being_skipped() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for i in 0..3 {
            ob.push(&req(&format!("id-{i}"), "x")).expect("排队");
        }
        // 把第二行整体换成垃圾
        let path = dir.path().join(QUEUE_FILE);
        let content = std::fs::read_to_string(&path).expect("读");
        let mut lines: Vec<&str> = content.lines().collect();
        lines[1] = "{这不是合法 JSON";
        std::fs::write(&path, lines.join("\n") + "\n").expect("写回");

        let err = ob
            .pending()
            .expect_err("中间损坏必须报错，不能跳过那一行继续");
        let msg = err.to_string();
        assert!(
            msg.contains("第 2 行"),
            "错误必须指明是哪一行，否则人工检查无从下手。实际：{msg}"
        );
        assert!(
            msg.contains("静默丢掉一轮对话"),
            "错误必须说清后果，否则第一反应是「加个跳过就好了」。实际：{msg}"
        );
    }

    /// 全部刷完后文件被截断，队列不会无界增长。
    #[test]
    fn a_fully_flushed_queue_truncates_itself() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for i in 0..3 {
            ob.push(&req(&format!("id-{i}"), "x")).expect("排队");
        }
        ob.commit(3).expect("全部刷出");
        assert_eq!(ob.backlog(), 0);
        let size = std::fs::metadata(dir.path().join(QUEUE_FILE))
            .expect("元数据")
            .len();
        assert_eq!(size, 0, "全部刷完后必须截断，否则文件无界增长");

        // 截断之后还能继续排队 —— 这是断网/联网反复切换时的常态
        ob.push(&req("id-after", "又断了")).expect("截断后仍能排队");
        assert_eq!(ob.pending().expect("读").len(), 1);
    }

    /// 高水位文件损坏时归零全量重放，**不能**猜一个偏大的值。
    ///
    /// 偏小只是多几次幂等的 HTTP；偏大会永久跳过中间那几条，
    /// 而它们再也不会被重放。
    #[test]
    fn an_unreadable_mark_falls_back_to_replaying_everything() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ob = Outbox::new(dir.path());
        for i in 0..3 {
            ob.push(&req(&format!("id-{i}"), "x")).expect("排队");
        }
        ob.commit(2).expect("提交");
        std::fs::write(dir.path().join(MARK_FILE), "垃圾").expect("破坏高水位");

        assert_eq!(
            ob.pending().expect("读").len(),
            3,
            "高水位读不出来时必须全量重放 —— 偏大会永久跳过中间那几条"
        );
    }
}
