//! 先摊开账，再动手。
//!
//! # 为什么 dry-run 是默认
//!
//! 每一对消息触发一次抽取，也就是一次 LLM 调用。三年历史可能是几千次 ——
//! 那是真金白银，而且跑起来之后没有「撤销」这个操作（记忆是 append-only，
//! 撤销要走 redact，而它要显式触发、二次确认、留墓碑）。
//!
//! 所以默认只算不做：把对话数、消息数、会触发多少次抽取、多少 token、
//! 时间跨度全摆出来，然后停下。真跑要显式 `--confirm`。
//!
//! # 为什么报 token 而不是报钱
//!
//! 各家定价不同、还在变，而且抽取走的是**服务端配的**廉价模型 ——
//! 客户端根本不知道那是哪个。硬编一个价格只会给出一个看起来精确的错数字。
//! 报 token，另给 `--price-per-1m` 让知道自己价格的人填。

use std::io::Write as _;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};

use super::{Conversation, Platform, requests_for};
use crate::client::Client;

/// 两次投递之间至少隔这么久。
///
/// 服务端有抽取并发闸门，但**那不是背压** —— 灌得太快只会让队列长在服务端
/// 内存里，而生产那台是 2 核 3.5 GB。这里主动慢下来，是客户端该做的事。
///
/// 150ms ≈ 每秒 6~7 对。三年历史（假设 5000 对）大约 15 分钟 ——
/// 一次性操作，这个速度完全可以接受，而它让服务端始终有余量伺候正常对话。
const PACE: Duration = Duration::from_millis(150);

/// 一次导入要做什么。
pub struct Plan {
    pub platform: Platform,
    pub conversations: Vec<Conversation>,
}

/// 摊开的账。
pub struct Estimate {
    pub conversations: usize,
    pub messages: usize,
    /// 会触发多少次抽取 = 多少次 LLM 调用
    pub pairs: usize,
    /// 送进抽取的文本量（粗估）
    pub tokens: usize,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    /// 有多少条消息落了单（进 episodes，但不产生事实）
    pub unpaired: usize,
}

impl Plan {
    #[must_use]
    pub fn estimate(&self) -> Estimate {
        let mut e = Estimate {
            conversations: self.conversations.len(),
            messages: 0,
            pairs: 0,
            tokens: 0,
            earliest: None,
            latest: None,
            unpaired: 0,
        };
        for c in &self.conversations {
            e.messages += c.messages.len();
            let pairs = c.pairs();
            e.pairs += pairs.len();
            e.unpaired += c.messages.len() - pairs.len() * 2;
            for (u, a) in pairs {
                // 与服务端拼给模型的那一段同构（`用户：…\n助手：…`），
                // 所以这个数字是「真的会被送进去的量」，不是文件大小
                e.tokens += cortex_core::injection::estimate_tokens(&u.text)
                    + cortex_core::injection::estimate_tokens(&a.text);
            }
            for m in &c.messages {
                e.earliest = Some(e.earliest.map_or(m.at, |x: DateTime<Utc>| x.min(m.at)));
                e.latest = Some(e.latest.map_or(m.at, |x: DateTime<Utc>| x.max(m.at)));
            }
        }
        e
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
    #[allow(clippy::cast_precision_loss)]
    let minutes = (e.pairs as f64 * PACE.as_secs_f64() / 60.0).ceil();
    println!("预计耗时至少 {minutes:.0} 分钟（客户端主动限速，避免压垮服务端）");
}

/// 真跑。
///
/// # 断点续传是免费的
///
/// episode id 由 [`cortex_core::Id::derived`] 从「原始时刻 + 稳定种子」算出，
/// 而 `POST /episodes` 按 id 判重。所以中断之后重跑同一条命令即可 ——
/// 已经写过的会被服务端识别出来（`already_existed`），不重复计费。
/// 不需要任何进度文件，也就没有「进度文件与实际状态不一致」这条故障。
pub async fn execute(client: &Client, plan: &Plan) -> Result<()> {
    let total = plan.conversations.len();
    let mut done_pairs = 0usize;
    let mut skipped = 0usize;
    let mut failures = 0usize;

    for (i, conv) in plan.conversations.iter().enumerate() {
        let pairs = conv.pairs();
        if pairs.is_empty() {
            continue;
        }
        let session = conv.session_id(plan.platform).to_string();

        for (u, a) in pairs {
            let [user_req, asst_req] = requests_for(plan.platform, conv, u, a);
            // user 必须先落：assistant 那条的 anchor 指向它，而服务端会拿
            // anchor 去库里读回 user 的原文来拼抽取输入。反过来的话，
            // 抽取会因为「找不到对应的 user episode」而整轮跳过
            match client.write_episode(&user_req).await {
                Ok(ack) if ack.already_existed => skipped += 1,
                Ok(_) => {}
                Err(e) => {
                    failures += 1;
                    tracing::warn!(error = %e, "写 user episode 失败，跳过这一对");
                    continue;
                }
            }
            match client.write_episode(&asst_req).await {
                Ok(ack) => {
                    if ack.already_existed {
                        skipped += 1;
                    }
                    done_pairs += 1;
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!(error = %e, "写 assistant episode 失败");
                }
            }

            tokio::time::sleep(PACE).await;
        }

        // 标题放在最后：会话要先有内容，改名才有东西可改。
        // 失败不算致命 —— 内容已经进去了，少一个标题而已
        if let Err(e) = client
            .rename_session(&session, &conv.display_title(plan.platform))
            .await
        {
            tracing::debug!(error = %e, session, "会话改名失败");
        }

        print!("\r  {} / {total} 段对话 · {done_pairs} 对已灌入", i + 1);
        let _ = std::io::stdout().flush();
    }
    println!();

    if skipped > 0 {
        println!("其中 {skipped} 条此前已经导过，服务端直接跳过了（没有重复计费）");
    }
    if failures > 0 {
        println!(
            "有 {failures} 次写入失败。**重跑同一条命令即可** —— \
             已经写进去的不会重复，只补没成的那些"
        );
    }
    Ok(())
}

/// 读文件并解析成 `Plan`，顺带做时间与数量的裁剪。
///
/// # Errors
/// 文件读不了、解析不了，或者裁剪之后一段对话都不剩。
pub fn load(
    path: &std::path::Path,
    platform: Option<Platform>,
    since: Option<DateTime<Utc>>,
    max: Option<usize>,
) -> Result<Plan> {
    let bytes = std::fs::read(path).with_context(|| format!("读不了 {}", path.display()))?;
    let (platform, mut conversations) = super::parse(&bytes, platform)?;

    if let Some(since) = since {
        conversations.retain(|c| c.messages.iter().any(|m| m.at >= since));
    }
    // 新的排前面：`--max-conversations 5` 试水时，要试的显然是最近的那几段
    conversations.sort_by_key(|c| std::cmp::Reverse(c.at));
    if let Some(n) = max {
        conversations.truncate(n);
    }

    if conversations.is_empty() {
        bail!("按当前的 --since / --max-conversations 筛完之后一段对话都不剩");
    }
    Ok(Plan {
        platform,
        conversations,
    })
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
    use crate::import::{Message, Role};

    fn conv(n_pairs: usize, extra_dangling: bool) -> Conversation {
        let mut messages = Vec::new();
        for i in 0..n_pairs {
            messages.push(Message {
                source_id: format!("u{i}"),
                role: Role::User,
                text: "问一个不长不短的问题".into(),
                at: format!("2024-03-0{}T10:00:00Z", i + 1).parse().unwrap(),
            });
            messages.push(Message {
                source_id: format!("a{i}"),
                role: Role::Assistant,
                text: "给一个不长不短的回答".into(),
                at: format!("2024-03-0{}T10:00:01Z", i + 1).parse().unwrap(),
            });
        }
        if extra_dangling {
            messages.push(Message {
                source_id: "dangling".into(),
                role: Role::User,
                text: "最后一问没人答".into(),
                at: "2024-03-09T10:00:00Z".parse().unwrap(),
            });
        }
        Conversation {
            source_id: "c".into(),
            title: "t".into(),
            at: "2024-03-01T10:00:00Z".parse().unwrap(),
            messages,
        }
    }

    /// 账要算对：**抽取次数等于对数**，不是消息数。
    ///
    /// 这个数字直接就是钱。按消息数报会让用户以为要花两倍。
    #[test]
    fn the_estimate_counts_extraction_calls_not_messages() {
        let plan = Plan {
            platform: Platform::ChatGpt,
            conversations: vec![conv(3, false), conv(2, false)],
        };
        let e = plan.estimate();
        assert_eq!(e.conversations, 2);
        assert_eq!(e.messages, 10);
        assert_eq!(e.pairs, 5, "5 对 = 5 次 LLM 调用，不是 10 次");
        assert!(e.tokens > 0);
        assert_eq!(e.unpaired, 0);
    }

    /// 落单的消息要单独报出来。
    ///
    /// 它们会写进原文却不产生任何事实 —— 用户看到「导入了 100 条消息」
    /// 却只多了 40 条记忆时，需要有个地方解释这个差。
    #[test]
    fn dangling_messages_are_reported_separately() {
        let plan = Plan {
            platform: Platform::Claude,
            conversations: vec![conv(2, true)],
        };
        let e = plan.estimate();
        assert_eq!(e.pairs, 2);
        assert_eq!(e.unpaired, 1, "最后那一问没人答，要报出来");
    }

    /// 时间跨度取的是**消息**的时刻，不是对话的创建时刻。
    #[test]
    fn the_span_comes_from_the_messages() {
        let plan = Plan {
            platform: Platform::ChatGpt,
            conversations: vec![conv(3, false)],
        };
        let e = plan.estimate();
        assert_eq!(
            e.earliest.unwrap().format("%Y-%m-%d").to_string(),
            "2024-03-01"
        );
        assert_eq!(
            e.latest.unwrap().format("%Y-%m-%d").to_string(),
            "2024-03-03"
        );
    }

    #[test]
    fn large_numbers_are_readable() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
