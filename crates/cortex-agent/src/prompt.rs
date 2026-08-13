//! 贴进系统提示词的**环境说明** —— 「你站在哪、这里有什么规矩」。
//!
//! # 为什么需要单独一族函数
//!
//! 模型看不见部署形态。它只看得见一条命令失败了，而失败的**理由**如果没有
//! 一起到达，它唯一能做的就是原样重试 —— 于是浪费一整轮，再撞同一堵墙。
//! 这个模块存放的就是「把墙的形状提前讲清楚」的那些话。
//!
//! # 为什么在 cortex-agent 而不是各自的宿主里
//!
//! 与 [`crate::workspace::brief`] 同一条理由：cortexd 与 cortex-local 各有
//! 一份 base 提示词且**已经在漂**，各自再贴一段环境说明只会漂得更开。
//! 而 [`crate::ExecEnvironment`] 本来就住在这个 crate —— 说明既然是环境的
//! 函数，它就该和环境放在一起。
//!
//! `brief` 没有搬进来，是因为 cortexd 正按 `workspace::brief` 这个路径引用它，
//! 为一次搬家去改另一个 crate 的 import 不划算。新的一律往这里放。

use crate::ExecEnvironment;

/// 容器里那层出网代理的说明。**非容器一律返回 `None`。**
///
/// # 这段话防的是哪次浪费
///
/// 沙箱的出网全部经 `cortex-egress`（默认全拒 + 放行清单）。明文 http 被拒时
/// 代理回的 403 正文里写清了「为什么 + 该怎么办」，那段中文原样进 curl 的
/// stdout，模型读得到；但 **https 走 `CONNECT`，curl 会丢弃失败 CONNECT 的
/// 响应体** —— 模型只剩一句 `curl: (56) CONNECT tunnel failed, response 403`。
/// 403 与 502 的区分还在（够判断「不该重试」），可「该换成哪个镜像源」丢了。
/// 实测过的输出就是那一行，没有别的。
///
/// 于是把缺的那半句提前讲：代理存在、403 是**策略**不是故障、换镜像源或者
/// 让用户改 `CORTEX_EGRESS_ALLOW`。
///
/// # 为什么不是「在 shell 输出里认签名再把说明贴回去」
///
/// 那是 `docs/sandbox.md` 第八节列的另一条路，看起来更精准，实际更脆：
///
/// * 认得出的只有 curl 一种措辞。pip 说 `ProxyError`、npm 说
///   `ECONNRESET`、git 说 `unable to access`，每加一个客户端就要补一条模式，
///   而**漏掉一条不会有任何症状** —— 正是这个仓库反复吃亏的形状。
/// * 被拦的域名在代理那一侧，命令文本里那个要靠解析才拿得到；解析 shell
///   命令行的东西一旦与真 shell 有分歧，分歧那一侧就是个静默的错误提示。
///
/// 提示词这条路不认任何签名，覆盖所有客户端，代价只是几十个 token 的
/// 可缓存前缀。
///
/// # 为什么桌面端必须是 `None`
///
/// 那边没有这层代理。印上去就是一句假话，而模型会照着它去解释一次**真的**
/// 网络故障（「哦，被策略拦了，不该重试」），把一个重试就能好的问题变成
/// 一个死结。
///
/// # 为什么按部署形态判断，而不是探一眼 `HTTPS_PROXY`
///
/// 同 [`ExecEnvironment::is_container`] 文档里的那条：探测出来的答案错了
/// 没人看得出来。entrypoint 哪天把变量名写成小写，这段话就**静默消失**，
/// 而症状是几个月后某一轮莫名其妙的重试。部署形态是显式传进来的，
/// 传错了 `--exec-env` 本身就会以别的方式炸出来。
#[must_use]
pub const fn egress_note(env: ExecEnvironment) -> Option<&'static str> {
    if !env.is_container() {
        return None;
    }
    Some(
        "关于联网：你跑在一次性容器里，容器的出网**全部经过一层放行清单代理**，\
         只有预先放行的域名连得上，其余一律拒绝。这是默认全拒的正常配置，不是故障。\n\
         - 看到 `CONNECT tunnel failed, response 403`，或者响应头里有 \
         `X-Cortex-Egress: blocked`，说明这次连接是**被策略拦下的**，不是网络坏了。\
         **不要重试同一个地址** —— 重试多少次都是同一个结果。改用已放行的镜像源；\
         确实非那个域名不可时，直接告诉用户把它加进 `CORTEX_EGRESS_ALLOW`。\n\
         - 明文 http 被拒时，拒绝理由的原文会直接出现在命令输出里，照它说的做。\
         https 走 CONNECT，curl 会丢掉那段正文，所以你只看得到上面那一行 —— \
         **看不到理由不等于没有理由**，别把它当成偶发故障。\n\
         - 502 是另一回事：地址放行了但连不上，那才是网络问题，可以重试一次。\n\
         - 绕过代理没有用。容器所在的网段没有第二条出口，清掉 `HTTP_PROXY` / \
         `HTTPS_PROXY` 只会让请求失败在路由上，错误信息还更难懂。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 桌面端印这段话就是撒谎，而撒的这个谎会让模型放弃重试一次**真的**故障。
    #[test]
    fn only_the_container_gets_the_egress_note() {
        assert!(
            egress_note(ExecEnvironment::Container).is_some(),
            "容器里有那层代理，不讲清楚的话模型看到 403 只会原样重试"
        );
        for env in [ExecEnvironment::LocalMachine, ExecEnvironment::None] {
            assert!(
                egress_note(env).is_none(),
                "{} 上根本没有出网代理，印这段话是假话 —— \
                 模型会照着它把一次真实的网络故障解释成「被策略拦了、不该重试」",
                env.as_str()
            );
        }
    }

    /// 这段话的全部价值在于「看到那个签名之后该干什么」。
    ///
    /// 逐条钉住而不是只测非空：少任何一条，模型都会退回原样重试 ——
    /// 而那种退化在日志里长得和「网络确实不好」一模一样。
    #[test]
    fn the_note_carries_every_piece_the_model_needs_to_change_course() {
        let note = egress_note(ExecEnvironment::Container).expect("容器模式必须有这段");
        for (needle, why) in [
            (
                "CONNECT tunnel failed, response 403",
                "这是 https 被拦时模型**唯一**看得到的字符串，\
                 提示词里不出现它，模型就没法把眼前那行和这段说明对上",
            ),
            (
                "X-Cortex-Egress: blocked",
                "非 curl 的客户端会把响应头透出来，那是第二个可认的签名",
            ),
            (
                "不要重试同一个地址",
                "整段话要改掉的行为就是这一个 —— 说别的都白搭",
            ),
            (
                "CORTEX_EGRESS_ALLOW",
                "得给模型一条出路：不是「你做不到」，而是「让用户把域名加进这里」",
            ),
            (
                "502",
                "403 与 502 必须分开讲。混为一谈的话，一次真实的连接失败\
                 会被当成策略拦截而放弃重试",
            ),
        ] {
            assert!(
                note.contains(needle),
                "出网说明里少了 {needle:?}：{why}。实际内容：{note}"
            );
        }
    }
}
