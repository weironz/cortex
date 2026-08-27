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

/// 技能目录 —— 贴进系统提示词的那一小块。**空清单一律返回 `None`。**
///
/// # 为什么这里只收 `(名字, 说明)`，而不是 `SkillBrief`
///
/// `cortex-agent` 不依赖 `cortex-proto`（线协议 crate 反过来也不该拖进整个
/// agent 循环，见 CLAUDE.md 的依赖方向）。收一对字符串就够了，而且它顺带
/// 让这个函数不必知道技能是从哪来的。
///
/// # ⚠️ 返回 `None` 时，`load_skill` 也**必须**从工具目录里拿掉
///
/// 两者同生共死（CLAUDE.md 约束 2）。只做一半的后果：
///
/// * 有目录没工具 → 模型看见「你可以取回技能」，却没有取的手段；
/// * 有工具没目录 → 模型手上有个不知道拿什么参数调的工具。
///
/// 两种都不报错，都表现为「模型胡说」。
///
/// # 为什么要写「先取回来再照着做」
///
/// 不写的话，模型会拿那一句话说明**当作技能本身**去做事 —— 它看起来像一条
/// 完整的指令（「按公司模板写周报」），于是它就照着自己脑补的模板写了。
/// 那次失败没有任何征兆：没有报错，只有一份不符合模板的周报。
#[must_use]
pub fn skills_note(catalog: &[(&str, &str)]) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    // ⚠️ **`concat!` 而不是跨行的裸字符串。**
    //
    // 这里从前是一个多行字面量，于是每行开头那 9 个缩进空格**是字符串的
    // 一部分**。而 markdown 里 4 个以上前导空格就是**缩进代码块** ——
    // 这段本该被当成指令读的话，模型看到的是一段示例代码。
    //
    // 更糟的是 `cargo fmt` 把原本分行的三句压成了一行，句与句之间留下
    // 9 空格的空档（2026-08-28 扫出来的；`b413dc6` 那次清扫漏了这一处）。
    // 也就是说这段**唯一要说的事**——「别拿一句话说明当技能本身，先
    // `load_skill` 把正文取回来」——是以代码块的形式讲的。
    let mut out = String::from(concat!(
        "# 可用技能\n",
        "\n",
        "下面每一条都是用户写好的一份做法，这里只列名字和用途。\n",
        "判断某一条与当前任务相关时，**先用 `load_skill` 把正文取回来，",
        "再照着做** —— 不要拿这里的一句话说明当作技能本身，它只是索引。\n",
        "\n",
    ));
    for (name, description) in catalog {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let description = description.trim();
        if description.is_empty() {
            out.push_str(&format!(
                "- `{name}`
"
            ));
        } else {
            out.push_str(&format!(
                "- `{name}`：{description}
"
            ));
        }
    }
    Some(out)
}

/// 电脑操作那一段说明。**只在这一组工具真的摆出来时才印。**
///
/// # 缺了它，模型会一直点偏
///
/// 截图是**缩放过的**（原图动辄 4K，塞进上下文要几万 token），而点击落在
/// 真实像素上。不讲这层换算的话，模型会照着自己看到的那张图的坐标去点，
/// 然后每一次都偏 —— 而它看不出为什么，只会重试。
///
/// 所以这段话把三件事讲死：**图上的坐标就是可以直接用的坐标**（换算由我们
/// 这一侧做，模型不必知道缩放比例）、先截图再动手、以及有哪些事不许做。
///
/// # 「不许做」那几条也在这里，而不只是靠权限闸
///
/// 权限闸拦的是**这一次**该不该做，它不知道「这件事根本不该由 agent 代做」。
/// 输密码、点付款、发送 —— 这些即便用户在某一次点了「允许」，也该由他自己动手。
/// 提示词里讲清楚，模型就会主动停下来问，而不是等闸门去拦。
#[must_use]
pub fn computer_note(width: u32, height: u32) -> String {
    format!(
        "# 操作这台电脑\n\n\
         你能看见并操作用户的屏幕。屏幕是 {width}x{height}。\n\n\
         - **动手之前先 `screenshot`。** 你看不见屏幕，凭记忆猜坐标必定点偏。\
         每次点击、输入之后再截一张确认真的生效了 —— 界面常常没有按你预期的变。\n\
         - **截图里的坐标可以直接用。** 图会被缩小以便你看清，但你读到的坐标\
         就是 `click` / `scroll` 要的坐标，不需要自己换算。\n\
         - 点击落在**指针下面那个窗口**上。要操作另一个窗口，先点它一下让它到前面来。\n\
         - `type_text` 打在**当前焦点**处。先 `click` 到该输入的地方，\
         否则那段文字会落进一个你不知道的窗口。\n\n\
         ## 这几件事不要代做，交回给用户\n\n\
         - 输入密码、验证码、银行卡号、任何密钥；\n\
         - 点「付款」「确认转账」「删除」「发送」这类**做完就收不回**的按钮；\n\
         - 同意条款、授权第三方登录。\n\n\
         这不是权限问题 —— 就算用户批准了这一次，这些也该由他自己动手。\
         走到这一步就停下来，说清楚屏幕上是什么、下一步该点哪儿，让他来点。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **给模型看的每一段都不许有 4 空格以上的前导缩进。**
    ///
    /// markdown 里 4 个以上前导空格是**缩进代码块**。一段本该被当成指令
    /// 读的散文，缩进之后模型看到的是一段示例代码 —— 而它不会照着示例
    /// 代码改变行为。
    ///
    /// 这不是假想：`skills_note` 从前是一个跨行的裸字符串，每行开头那
    /// 9 个空格是字符串的一部分，于是「先用 `load_skill` 把正文取回来」
    /// 这句**唯一要紧的话**是以代码块的形式讲的（2026-08-28 扫出来的）。
    ///
    /// 这条盖住所有在这个文件里拼出来的提示词段。写新的那一段时，
    /// 用 `concat!` 逐行拼，别写跨行的裸字符串 —— `cargo fmt` 还会把
    /// 反斜杠续行压成一行并把缩进留成字面空格，那是同一个坑的另一面。
    #[test]
    fn 给模型看的每一段都不带markdown会当成代码块的缩进() {
        let mut notes: Vec<(&str, String)> = vec![
            (
                "skills_note",
                skills_note(&[("a", "用途一"), ("b", "用途二")]).expect("非空目录该有"),
            ),
            (
                "egress_note",
                egress_note(ExecEnvironment::Container)
                    .expect("容器该有")
                    .to_string(),
            ),
        ];
        notes.push(("computer_note", computer_note(1920, 1080)));

        for (which, text) in notes {
            for (n, line) in text.lines().enumerate() {
                let indent = line.len() - line.trim_start().len();
                assert!(
                    indent < 4 || line.trim().is_empty(),
                    "{which} 第 {} 行有 {indent} 个前导空格 —— markdown 会把它当成代码块，\
                     而模型不会照着示例代码改变行为：{line:?}",
                    n + 1
                );
            }
        }
    }

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
    /// 一条技能都没有时**什么都不印**。
    ///
    /// 印一句「你可以使用技能（当前没有）」的话，模型仍然会去调
    /// `load_skill` 试试 —— 那是 CLAUDE.md 约束 2 说的那种「说得到做不到」。
    #[test]
    fn no_skills_means_no_block_at_all() {
        assert!(
            skills_note(&[]).is_none(),
            "空目录必须整块不印 —— 连同 load_skill 一起消失，两者同生共死"
        );
    }

    /// 目录里要有名字、用途，以及**「先取回来」这条动作**。
    #[test]
    fn the_catalog_tells_the_model_to_fetch_before_acting() {
        let note = skills_note(&[("周报", "按公司模板写周报"), ("查数", "从内网报表取数")])
            .expect("非空目录必须印");
        assert!(note.contains("周报") && note.contains("按公司模板"));
        assert!(note.contains("查数"));
        assert!(
            note.contains("load_skill"),
            "不点名那个工具的话，模型不知道用什么去取"
        );
        assert!(
            note.contains("不要拿这里的一句话说明当作技能本身"),
            concat!(
                "缺这句时模型会照着说明脑补着做 —— 而那次失败没有任何征兆，",
                "只有一份不符合模板的周报",
            )
        );
    }

    /// 没写说明的技能仍然进目录 —— 名字本身往往就说明了用途。
    #[test]
    fn a_skill_without_a_description_still_gets_listed() {
        let note = skills_note(&[("周报", "  ")]).expect("非空目录");
        assert!(
            note.contains("`周报`"),
            "把它挡在外面等于用户配了个永远不出现的技能"
        );
        // 名字是空的那条要跳过：模型没法用空字符串去取它
        let only_blank = skills_note(&[("  ", "有说明没名字")]).expect("清单非空");
        assert!(
            !only_blank.contains("有说明没名字"),
            "没名字的那条取不回来，列出来只会诱导模型去调一个必然失败的调用"
        );
    }
    /// 那段说明要把「先截图」「坐标直接用」「哪些不许代做」三件事都讲到。
    ///
    /// 逐条钉住而不是只测非空：少任何一条都不会报错，只是模型的行为悄悄变坏
    /// —— 少第一条它凭空猜坐标，少第二条它自己去换算缩放比例（换算错了还
    /// 以为是界面的问题），少第三条它会替用户按下「确认转账」。
    #[test]
    fn the_computer_note_covers_what_the_model_cannot_infer() {
        let note = computer_note(1920, 1080);
        assert!(note.contains("1920x1080"), "得告诉它屏幕多大");
        for (needle, why) in [
            (
                "先 `screenshot`",
                "不讲的话它会凭记忆猜坐标 —— 而它根本没见过这块屏幕",
            ),
            (
                "不需要自己换算",
                "图是缩放过的。不讲这条，模型会自己去推缩放比例，推错了还以为是界面的问题",
            ),
            (
                "当前焦点",
                "type_text 打在焦点处。不讲的话那段文字会落进一个谁也不知道的窗口",
            ),
            (
                "密码",
                "替用户输密码是这一组工具里最该拦住的一件事，而权限闸拦不了它 —— \
                 闸只知道「这一次准不准」，不知道「这件事根本不该代做」",
            ),
            ("收不回", "付款、发送、删除这类按钮按下去就没有下一步了"),
        ] {
            assert!(
                note.contains(needle),
                "电脑操作说明里少了 {needle:?}：{why}。实际内容：{note}"
            );
        }
    }
}
