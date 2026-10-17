//! 打转守卫：同一个工具用**完全相同的参数**调了 N 次，就把这件事说出来。
//! 只提醒，不阻断 —— 见 [`RepeatWatch`] 的文档。

/// 「你已经把同一个工具用同样的参数调了 N 次了」。
///
/// # 在修什么
///
/// 模型会卡在原地：同一条失败的命令重跑、同一个没变过的文件重读。
/// `max_rounds` 把**成本**封了顶（8 轮），但它封不住**结果**：那一轮
/// 烧完八次模型调用，全花在同一个 grep 上，然后以「我没能完成」收场。
/// 用户看到的是「它想了很久然后说不行」。
///
/// 思路取自 DeepSeek Harness 的 `guard/repeat-tool-reminder`（MIT）——
/// 那边的定案有两条值得照抄：
///
/// 1. **只提醒，不阻断。** 合法的重复（轮询一个后台任务、按同样条件
///    再查一次）必须一次都不被拦。判断留给模型，我们只是把它看不见的
///    事实说出来：你已经这样做过 N 次了。
/// 2. **只认完全相同**（同工具 + 同参数，与键的顺序无关）。近似的变体
///    不算 —— 那需要猜「什么算变体」，而猜错的方向是拦下正当的工作。
///
/// # 为什么是每轮，不是每会话
///
/// 与 [`ConfirmState`] 同款论证：`Turn` 可复用且无每轮状态。而且
/// 「新的用户消息清零」在这一侧天然成立 —— 一轮就是一条用户消息。
#[derive(Debug, Default)]
pub(crate) struct RepeatWatch {
    /// `(工具名, 规范化参数)` → 已经调过几次。
    seen: std::collections::HashMap<(String, String), u32>,
}

/// 第几次重复时提醒。
///
/// 3 是「已经不像巧合了」，5 是「明显在原地打转」，8 是 [`DEFAULT_MAX_ROUNDS`]
/// —— 到这儿这一轮已经走到头，最后一次提醒等于告诉它「收尾吧」。
///
/// 一轮里的**工具调用**次数可以多于轮数（一轮能并排调好几个），
/// 所以 8 这一档不是走不到的死数。
const REPEAT_REMINDERS: [u32; 3] = [3, 5, 8];

/// 这些工具**本来就该**被重复调用，重复不是打转。
///
/// - `todo_write`：每次更新清单都是同一个工具，参数还常常只差一个字
/// - `background_output`：轮询一个后台任务，参数完全相同**才是对的用法**
///
/// 漏掉这两个的后果不是「少提醒一次」，是**在用户正常使用时插一句
/// 「你是不是卡住了」** —— 而模型会照着这句话改变行为。
const REPEAT_EXEMPT: [&str; 2] = ["todo_write", "background_output"];

impl RepeatWatch {
    /// 记一次调用，回一句要附在结果后面的提醒（没到阈值就是 `None`）。
    pub(crate) fn note(&mut self, tool: &str, arguments: &serde_json::Value) -> Option<String> {
        if REPEAT_EXEMPT.contains(&tool) {
            return None;
        }
        let key = (tool.to_owned(), canonical_args(arguments));
        let n = self.seen.entry(key).or_insert(0);
        *n += 1;
        let n = *n;
        REPEAT_REMINDERS.contains(&n).then(|| {
            format!(
                "\n\n[提醒] 你已经用**完全相同的参数**调了 {tool} {n} 次，而结果没有变。\
                 先读一遍上一次的结果：如果它已经回答了问题，就别再调了；\
                 如果没有，换个做法（换参数、换工具、或者直接告诉用户你卡在哪儿）。\
                 这只是提醒，不是拦截 —— 确有必要的话你仍然可以再调一次。"
            )
        })
    }
}

/// 把参数规范化成一个可比较的字符串：**对象的键排序**。
///
/// 不排的话 `{"a":1,"b":2}` 与 `{"b":2,"a":1}` 是两次不同的调用，而模型
/// 每次生成的键顺序并不稳定 —— 于是一个真的在打转的循环一次都不会被认
/// 出来，守卫看起来在跑，实际一次都不触发。
///
/// ⚠️ **这个 `sort()` 今天是空转，留着是防一个 feature 开关。**
/// `serde_json` 默认的 `Map` 背后是 `BTreeMap`，键本来就有序；只有别人
/// 打开 `preserve_order`（换成 `IndexMap`，按插入序）时它才开始起作用。
/// 实测过：把 `sort()` 删掉，那条键序测试照样绿 —— 也就是说**这条不变量
/// 今天没有测试守得住**，写在这儿免得下一个人以为它有覆盖。
fn canonical_args(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{k}:{}", canonical_args(&m[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => {
            // 数组**不排序**：`["a","b"]` 与 `["b","a"]` 对多数工具是两件事
            let parts: Vec<String> = a.iter().map(canonical_args).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **原地打转要被说出来，而合法的重复一次都不许被打扰。**
    ///
    /// `max_rounds` 封的是成本，封不住结果：一轮八次模型调用全花在同一个
    /// grep 上，然后以「我没能完成」收场。用户看到的是「它想了很久说不行」。
    #[test]
    fn 同样的调用重复到阈值才提醒_而且只是提醒() {
        let mut w = RepeatWatch::default();
        let args = serde_json::json!({"pattern": "foo", "path": "src"});

        assert!(w.note("grep", &args).is_none(), "第 1 次是正常工作");
        assert!(w.note("grep", &args).is_none(), "第 2 次也还不是打转");
        let third = w.note("grep", &args).expect("第 3 次该提醒了");
        assert!(third.contains("3 次"), "要说出次数：{third}");
        assert!(
            third.contains("不是拦截"),
            "必须说清它只是提醒 —— 否则模型会把它读成「不许再调」：{third}"
        );

        assert!(w.note("grep", &args).is_none(), "第 4 次不再唠叨");
        assert!(w.note("grep", &args).is_some(), "第 5 次是下一档");
    }

    /// **键的顺序不算差别。**
    ///
    /// 模型每次生成的键顺序并不稳定；按字面比的话，一个真的在打转的循环
    /// 一次都不会被认出来 —— 守卫看起来在跑，实际一次都不触发。
    ///
    /// ⚠️ **这一条今天恒绿，删掉 `canonical_args` 里那个 `sort()` 也不会
    /// 红**（实测过）：`serde_json` 默认的 `Map` 是 `BTreeMap`，键本来就
    /// 有序。它守的是「哪天有人打开 `preserve_order`」那个未来 ——
    /// 写清楚，免得它被当成一条有效的防线。
    #[test]
    fn 参数只是键序不同也算同一次调用() {
        let mut w = RepeatWatch::default();
        for args in [
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({"b": 2, "a": 1}),
            serde_json::json!({"a": 1, "b": 2}),
        ] {
            let hit = w.note("read_file", &args);
            if hit.is_some() {
                return; // 第三次触发了 —— 正是要的
            }
        }
        panic!("键序不同被当成了不同的调用，于是打转永远认不出来");
    }

    /// 参数**真的**不同就不是重复。
    #[test]
    fn 参数不同不算重复() {
        let mut w = RepeatWatch::default();
        for i in 0..6 {
            assert!(
                w.note(
                    "read_file",
                    &serde_json::json!({"path": format!("f{i}.txt")})
                )
                .is_none(),
                "读六个不同的文件是正常工作，不是打转"
            );
        }
        // 数组顺序是有意义的：`["a","b"]` 与 `["b","a"]` 对多数工具是两件事
        let mut w2 = RepeatWatch::default();
        for _ in 0..3 {
            w2.note("t", &serde_json::json!({"xs": ["a", "b"]}));
        }
        assert!(
            w2.note("t", &serde_json::json!({"xs": ["b", "a"]}))
                .is_none(),
            "数组换了顺序不该被算成同一次调用"
        );
    }

    /// **本来就该重复的工具一次都不许被提醒。**
    ///
    /// 轮询一个后台任务，参数完全相同才是对的用法。在这里插一句
    /// 「你是不是卡住了」，模型会照着改变行为 —— 那是凭空制造的故障。
    #[test]
    fn 该重复的工具不提醒() {
        for tool in REPEAT_EXEMPT {
            let mut w = RepeatWatch::default();
            for i in 0..12 {
                assert!(
                    w.note(tool, &serde_json::json!({})).is_none(),
                    "{tool} 第 {} 次被提醒了 —— 它本来就该被反复调用",
                    i + 1
                );
            }
        }
    }
}
