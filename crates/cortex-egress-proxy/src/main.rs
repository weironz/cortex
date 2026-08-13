//! `cortex-egress-proxy` —— 沙箱网段唯一的**双宿**出口。
//!
//! # 为什么需要它（一句话）
//!
//! 沙箱网段是 `internal: true`：容器里**连默认路由都没有**。这不是「设了
//! `HTTP_PROXY` 请大家自觉走代理」，是物理上没有第二条路。
//!
//! ```text
//!   [cortex-sandbox-net: internal]          [cortex-egress-net: bridge]
//!    沙箱容器 ──出网──► cortex-egress ──────────► 宿主 / 放行的外网
//!        ▲                    │
//!        └── cortexd 反代进来 ┘  （中继的已发布端口）
//! ```
//!
//! # 为什么两个方向都由它做
//!
//! 实测过（记在 `docs/sandbox.md` 第八节）：**内部网段上已发布端口失效**。
//! 于是 cortexd 那侧原来「连 `127.0.0.1:<容器映射端口>`」的路走不通了。
//! 与其另造一个中继，不如让这个已经双宿的容器把两个方向都担下来 ——
//! 它本来就是这张拓扑上唯一一个两边都够得着的点。
//!
//! # 为什么不是 nginx / envoy
//!
//! 要的是「拒绝理由必须回到 agent，让模型自己换路」，而不是一个 403 空响应。
//! 那句解释是 `allowlist::Verdict::explain` 生成的，配置文件里写不出来。
//!
//! # 这里**不做** TLS 终止
//!
//! CONNECT 级放行只看得见域名与端口，看不见路径与内容 —— 这是刻意的：
//! 终止 TLS 意味着要在容器里塞一个自签 CA，那等于把「沙箱看不到用户凭据」
//! 这条性质亲手拆掉。宁可粒度粗一点。

mod allowlist;
mod denials;
mod inbound;
mod outbound;

use std::sync::Arc;

use allowlist::Allowlist;

/// 出网代理（沙箱侧）监听的端口。`HTTP_PROXY` 指向它。
const FORWARD_PORT: u16 = 3128;
/// 反向中继（cortexd 侧）监听的端口。这一个会被 publish 到宿主。
const RELAY_PORT: u16 = 3129;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cortex_egress_proxy=info".into()),
        )
        .init();

    let list = Arc::new(Allowlist::new(
        &std::env::var("CORTEX_EGRESS_ALLOW").unwrap_or_default(),
        &std::env::var("CORTEX_EGRESS_DENY").unwrap_or_default(),
    ));

    if list.is_empty() {
        // 不是 fatal：setup 阶段之外，一个「什么都不放行」的沙箱是合法形态
        // （纯本地任务）。但它必须在日志第一屏就说清楚 —— 否则症状是
        // 「agent 什么都下不下来」，而人第一反应是去查代理起没起
        tracing::warn!(
            "放行清单是空的 —— 沙箱将无法访问任何外网地址。\
             这是「默认全拒」的正常结果，不是故障。要放行请设 CORTEX_EGRESS_ALLOW"
        );
    }

    let denials = Arc::new(denials::Denials::new());
    let forward = tokio::spawn(outbound::serve(FORWARD_PORT, Arc::clone(&list), denials));
    let relay = tokio::spawn(inbound::serve(RELAY_PORT));

    // 任何一半退出都让整个进程退出：只剩一半活着的代理，症状是
    // 「有时候能连有时候不能」，比直接死掉难查得多
    tokio::select! {
        r = forward => tracing::error!(?r, "出网代理退出"),
        r = relay   => tracing::error!(?r, "反向中继退出"),
    }
    std::process::exit(1);
}
