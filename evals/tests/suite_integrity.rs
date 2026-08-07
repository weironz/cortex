//! 对仓库内自建题集本身的检查。
//!
//! 题集是评测的地基，地基坏了所有数字都是假的。这些检查**不需要数据库**，
//! 因此可以无条件进 CI —— 它们挡住的是「题目写错了」这一类最容易发生、
//! 又最难从报告里看出来的问题。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use cortex_evals::matcher::Matcher;
use cortex_evals::suite::{Lang, QuestionKind, Suite};

fn suite_path() -> PathBuf {
    // 测试的工作目录是 crate 目录（evals/）
    PathBuf::from("suites/cortex-zh-en-v1.json")
}

fn load() -> Suite {
    Suite::load(&suite_path()).expect("仓库内的题集必须能加载并通过静态校验")
}

/// 题集里全部铺垫事实的 statement —— gold 就是往这些串上匹配的。
fn statements(suite: &Suite) -> Vec<String> {
    suite
        .scenarios
        .iter()
        .flat_map(|s| &s.turns)
        .flat_map(|t| &t.facts)
        .map(|f| f.statement.clone())
        .collect()
}

#[test]
fn bundled_suite_loads_and_validates() {
    let suite = load();
    assert!(
        suite.questions.len() >= 60,
        "题量少于 60 时分组指标的每一格都只剩个位数，统计上没有意义，实际 {} 题",
        suite.questions.len()
    );
}

#[test]
fn every_gold_matcher_hits_at_least_one_seeded_fact() {
    // seed 模式下 gold 匹配不到任何铺垫事实，这道题就是恒定 0 分的死题 ——
    // 它会把基线数字压低，而且压低的原因看报告根本看不出来
    let suite = load();
    let stmts = statements(&suite);

    let mut orphans = Vec::new();
    for q in &suite.questions {
        for group in &q.gold {
            let m = Matcher::new(group);
            if !stmts.iter().any(|s| m.matches(s)) {
                orphans.push(format!("{} → {}", q.id, m.describe()));
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "以下 gold 匹配不到任何铺垫事实（题目写错了，不是检索的问题）：\n{}",
        orphans.join("\n")
    );
}

#[test]
fn every_forbidden_matcher_hits_at_least_one_seeded_fact() {
    // forbidden 匹配不到任何东西 = 这条断言永远为真 = 白写。
    // 「应当召回不到」那一类全靠 forbidden 支撑，形同虚设就等于没测
    let suite = load();
    let stmts = statements(&suite);

    let mut orphans = Vec::new();
    for q in &suite.questions {
        for group in &q.forbidden {
            let m = Matcher::new(group);
            if !stmts.iter().any(|s| m.matches(s)) {
                orphans.push(format!("{} → {}", q.id, m.describe()));
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "以下 forbidden 匹配不到任何铺垫事实，这条断言恒为真：\n{}",
        orphans.join("\n")
    );
}

#[test]
fn gold_and_forbidden_never_overlap() {
    // 同一条事实既是 gold 又是 forbidden，这道题无论如何都判不对
    let suite = load();
    let stmts = statements(&suite);

    for q in &suite.questions {
        for g in &q.gold {
            let gm = Matcher::new(g);
            for f in &q.forbidden {
                let fm = Matcher::new(f);
                let clash = stmts.iter().any(|s| gm.matches(s) && fm.matches(s));
                assert!(
                    !clash,
                    "题目 {} 的 gold「{}」与 forbidden「{}」命中了同一条事实",
                    q.id,
                    gm.describe(),
                    fm.describe()
                );
            }
        }
    }
}

#[test]
fn every_kind_has_enough_questions_to_be_readable() {
    // 分组指标的意义在于「中文语义类 R@5 只有 0.3」这种可行动的结论。
    // 一类只有两三题时，一道题的抖动就是 30 个百分点
    let suite = load();
    for (kind, n) in suite.count_by_kind() {
        assert!(
            n >= 8,
            "题型「{}」只有 {n} 题，单题抖动会淹没信号",
            kind.label()
        );
    }
}

#[test]
fn suite_is_bilingual() {
    // 自建集的存在理由之一就是公开基准全是英文；反过来也不能只有中文
    let suite = load();
    let by_lang = suite.count_by_lang();
    for lang in [Lang::Zh, Lang::En, Lang::Mixed] {
        assert!(
            by_lang.get(&lang).copied().unwrap_or(0) >= 5,
            "语种「{}」题量不足：{:?}",
            lang.label(),
            by_lang
        );
    }
}

#[test]
fn temporal_questions_reference_distinct_checkpoints() {
    // 全部回放题都指向同一个时点的话，测的只是「当前有效集」，不是双时间轴
    let suite = load();
    let used: HashSet<&String> = suite
        .questions
        .iter()
        .filter(|q| q.kind == QuestionKind::TemporalReplay)
        .filter_map(|q| q.as_of.as_ref())
        .collect();
    assert!(
        used.len() >= 3,
        "时间回放题只用了 {} 个 checkpoint，覆盖不到多个认知时点",
        used.len()
    );
}

#[test]
fn superseding_facts_share_subject_and_predicate() {
    // 取代关系靠 (subject, predicate) 撞车 + valid_at 分先后来成立。
    // 谓词不在 `PredicateRules` 的单值表里，或者两条的 valid_at 相同，
    // 取代就不会发生，回放题会静默地全部答成「两条都在」
    let suite = load();
    let mut slots: HashMap<(String, String), Vec<Option<String>>> = HashMap::new();
    for f in suite
        .scenarios
        .iter()
        .flat_map(|s| &s.turns)
        .flat_map(|t| &t.facts)
    {
        slots
            .entry((f.subject.clone(), f.predicate.clone()))
            .or_default()
            .push(f.valid_at.clone());
    }

    // 题集刻意安排的四组取代
    for (subject, predicate) in [
        ("对象存储", "stores_in"),
        ("存储层", "assigned_to"),
        ("v1 发布", "deadline"),
        ("同步协议", "decided_on"),
    ] {
        let key = (subject.to_owned(), predicate.to_owned());
        let dates = slots
            .get(&key)
            .unwrap_or_else(|| panic!("题集里应当有 {subject}/{predicate} 这一组取代"));
        assert_eq!(
            dates.len(),
            2,
            "{subject}/{predicate} 应当正好两条（旧值 + 新值），实际 {}",
            dates.len()
        );
        let mut sorted: Vec<&Option<String>> = dates.iter().collect();
        sorted.sort();
        assert!(
            sorted[0] < sorted[1],
            "{subject}/{predicate} 的两条 valid_at 必须分得出先后，否则只会被打成 flag 而不是取代"
        );
    }
}

#[test]
fn unanswerable_questions_have_tempting_distractors() {
    // 「应当召回不到」题的价值全在干扰项的诱惑力上。
    // 干扰项太少，这一类就退化成「随便问句废话」
    let suite = load();
    for q in &suite.questions {
        if q.kind == QuestionKind::Unanswerable {
            assert!(
                !q.forbidden.is_empty(),
                "题目 {} 没有干扰项，测不出任何东西",
                q.id
            );
        }
    }
}

#[test]
fn question_ids_are_sorted_and_dense() {
    // 报告与基线 JSON 要能直接 diff，id 乱序会让 diff 满屏噪声
    let suite = load();
    let ids: Vec<&str> = suite.questions.iter().map(|q| q.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "题目 id 应当按字典序排列，便于比对报告");
}
