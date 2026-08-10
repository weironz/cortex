//! 把 [`cortex_import`] 的账渲染成终端输出，并把 CLI 的 HTTP 客户端
//! 接成它的 [`Sink`]。
//!
//! # 这里为什么只剩渲染
//!
//! 解析、配对、算账、限速、写入顺序全在 `cortex-import` 里 ——
//! 因为桌面端与网页端也要跑同一套。留在这儿的只有两件与终端有关的事：
//! 把数字排版成人话，以及把进度打在同一行上。
//!
//! # 为什么报 token 而不是报钱
//!
//! 各家定价不同、还在变，而且抽取走的是**服务端配的**廉价模型 ——
//! 客户端根本不知道那是哪个。硬编一个价格只会给出一个看起来精确的错数字。
//! 报 token，另给 `--price-per-1m` 让知道自己价格的人填。

use std::io::Write as _;

use anyhow::Result;
use cortex_import::{Plan, Progress, Sink};
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};

use crate::client::Client;

/// 把 CLI 那个 HTTP 客户端接成导入的出口。
struct CliSink<'a>(&'a Client);

impl Sink for CliSink<'_> {
    async fn write_episode(&self, req: &NewEpisodeRequest) -> Result<EpisodeAck> {
        // CortexError → anyhow：trait 用 anyhow 是因为三个实现方
        // （CLI / 本地 agent / cortexd）各有各的错误类型，统一到一个
        // 具体类型只会逼另外两方也依赖它
        Ok(self.0.write_episode(req).await?)
    }

    async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        Ok(self.0.rename_session(session_id, title).await?)
    }
}

/// 把账打印出来。
pub fn report(plan: &Plan, price_per_1m: Option<f64>) {
    let e = plan.estimate();
    println!("来源：{}", plan.platform.label());
    println!("对话：{} 段 · 消息：{} 条", e.conversations, e.messages);
    if let (Some(a), Some(b)) = (e.earliest, e.latest) {
        println!(
            "时间跨度：{} → {}",
            a.format("%Y-%m-%d"),
            b.format("%Y-%m-%d")
        );
    }
    println!();
    println!("会触发 {} 次事实抽取（每次一回 LLM 调用）", e.pairs);
    println!("送进抽取的文本约 {} token", thousands(e.tokens));
    if let Some(p) = price_per_1m {
        #[allow(clippy::cast_precision_loss)]
        let cost = e.tokens as f64 / 1_000_000.0 * p;
        println!("按 {p}/百万 token 估算：约 {cost:.2}（只算输入，输出另计）");
    } else {
        println!("（要估费用就加 --price-per-1m，填你那个廉价模型的单价）");
    }
    if e.unpaired > 0 {
        println!();
        println!(
            "另有 {} 条消息落单（开头就是助手、或者最后一问没有回答）。\
             它们照样写进原文，只是不产生事实",
            e.unpaired
        );
    }
    println!();
    println!(
        "预计耗时至少 {:.0} 分钟（客户端主动限速，避免压垮服务端）",
        e.minutes()
    );
}

/// 真跑。断点续传是免费的 —— 见 [`cortex_import::execute`]。
///
/// # Errors
/// 目前只透传 [`cortex_import::execute`] 的错误。
pub async fn execute(client: &Client, plan: &Plan) -> Result<()> {
    let total = plan.conversations.len();
    let done = cortex_import::execute(&CliSink(client), plan, |p: Progress| {
        print!(
            "\r  {} / {total} 段对话 · {} 对已灌入",
            p.conversations_done, p.pairs_done
        );
        let _ = std::io::stdout().flush();
    })
    .await?;
    println!();

    if done.skipped > 0 {
        println!(
            "其中 {} 条此前已经导过，服务端直接跳过了（没有重复计费）",
            done.skipped
        );
    }
    if done.failures > 0 {
        println!(
            "有 {} 次写入失败。**重跑同一条命令即可** —— \
             已经写进去的不会重复，只补没成的那些",
            done.failures
        );
    }
    Ok(())
}

/// 千分位。几万这个量级的数字，不分位就读不出数量级。
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_numbers_are_readable() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
