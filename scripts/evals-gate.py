#!/usr/bin/env python3
# ══════════════════════════════════════════════════════════
#  scripts/evals-gate.py —— 检索评测的回归门
#
#  用法：
#    # 逐题型比对基线，跌破容差就以非零码退出
#    python scripts/evals-gate.py check \
#        --report evals-report.json \
#        --baseline scripts/evals-baseline.hash.json
#
#    # 把一次运行的结果存成新基线（调参定案后手工执行）
#    python scripts/evals-gate.py bless \
#        --report evals-report.json \
#        --baseline scripts/evals-baseline.hash.json
#
#    # 只打印扁平指标，不做判定
#    python scripts/evals-gate.py show --report evals-report.json
#
#  ── 为什么不能只比总分 ────────────────────────────────────
#
#  evals/README.md 已经把这件事说透了：第一份基线的「整体 R@5 = 0.78」
#  是「专名精确 0.92 + 中文语义 0.69 + 时间回放 0.42」平均出来的。
#  某一类塌掉而总分只微跌，只看总分的门会放它过去 ——
#  而「中文语义」正是这个项目的差异化卖点，塌了等于卖点没了。
#
#  `Report::gate_metrics()` 已经把该比的数字摊平成
#  `kind.中文语义.recall@5` 这样的扁平 map。本脚本从报告 JSON 里
#  按同样的规则重算这张表（不依赖 Rust 侧把它序列化出来），逐项 diff。
#
#  ── 容差怎么定 ────────────────────────────────────────────
#
#  seed 模式是确定性的（跑一百遍结果一致），所以没有「抖动」要容忍，
#  容差要容忍的是**有意的调参取舍**：改一个旋钮常常是拿这一类的一道题
#  换那一类的两道题。
#
#  于是按「几道题」而不是按固定小数来定：
#      每一类的容差 = max(FLOOR, 1.5 / 该类计分题数)
#  1.5 这个系数是算出来的，不是拍的：它让「掉一道题」恒在容差内、
#  「掉两道题」恒在容差外，对本题集全部题型规模（9 ~ 33 题）都成立。
#  用 2.0 会让 n=33 时「掉两道」正好等于容差而被放行 —— 差一点点的那种漏。
#  题数少的类（10 道的跨域检索）一道题就值 0.1，容差自然放宽到 0.15 ——
#  这是对的，小样本本该更宽容；FLOOR 只在大类（27+ 题）上起作用。
#
#  ── 只降不升会被记住 ──────────────────────────────────────
#
#  涨了不报错，但会在输出里显式列出来，提醒「该 bless 新基线了」。
#  基线长期停在旧数字上，门就会慢慢失去意义。
# ══════════════════════════════════════════════════════════

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# 题型标签是中文，Windows 控制台默认 cp936，直接 print 会变成乱码。
# CI 在 Linux/UTF-8 上没这问题，但开发者在 Git Bash 里跑同一个脚本时会。
for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError):
        pass

# 总分容差。97 道计分题里掉一道 ≈ 0.010、掉两道 ≈ 0.021，
# 0.02 正好卡在中间：掉一道放过，掉两道就红。与逐题型是同一套口径。
OVERALL_TOLERANCE = 0.02

# 逐题型容差的下限。题数多的类（27+ 题）靠它兜底 ——
# 1.5/33 = 0.045 太紧，一次无害的排序抖动就能踩到。
PER_KIND_FLOOR = 0.05

# 「掉一道放过、掉两道就红」的那个系数。见文件头的推导。
PER_KIND_QUESTIONS = 1.5

# 误召率是**越低越好**的指标，方向和 recall 相反，单独处理。
# 它是「防止检索器退化成什么都召回」的那把锁，只准降不准升太多。
FORBIDDEN_TOLERANCE = 0.08


def flatten(report: dict) -> dict[str, float]:
    """把报告摊平成扁平 map，规则与 Rust 侧 `Report::gate_metrics()` 一致。"""
    out: dict[str, float] = {}
    overall = report.get("overall", {})
    for k, v in (overall.get("recall") or {}).items():
        out[f"overall.recall@{k}"] = float(v)
    out["overall.mrr"] = float(overall.get("mrr", 0.0))
    out["overall.recall_any"] = float(overall.get("recall_any", 0.0))
    for agg in report.get("by_kind", []):
        label = agg.get("label", "?")
        # scored == 0 的题型（「应召不到」：gold 必须为空）recall 恒为 0，
        # 比它等于每次都拿 0 和 0 比 —— 白占一行还会掩盖真正该看的 forbidden。
        if int(agg.get("scored", 0) or 0) > 0:
            out[f"kind.{label}.recall@5"] = float((agg.get("recall") or {}).get("5", 0.0))
        fb = (agg.get("forbidden_rate") or {}).get("5")
        if fb is not None and agg.get("forbidden_questions", 0):
            out[f"kind.{label}.forbidden@5"] = float(fb)
        # ── 抽取侧的两条，越低越好 ──
        #
        # 加它们的理由：Recall 这一族的分母是「已经在库里的事实」，**结构上**
        # 回答不了「这条该不该被抽出来」。而 gold 缺失的题被排除在 Recall 之外
        # （「抽取的锅不算检索头上」），于是抽取侧的退化在这道门里**一格都不占**。
        #
        # 工具经验那 8 道题就是活证：它们现在 gold_missing = 8/8，
        # 但 scored == 0，所以 kind.工具经验.recall@5 压根不生成 ——
        # 门看不见它们存在。
        total = int(agg.get("total", 0) or 0)
        if total > 0:
            out[f"kind.{label}.gold_missing"] = float(agg.get("gold_missing", 0) or 0) / total
        leak_q = int(agg.get("leak_questions", 0) or 0)
        if leak_q > 0:
            out[f"kind.{label}.extraction_leak"] = (
                float(agg.get("extraction_leaks", 0) or 0) / leak_q
            )
    for agg in report.get("by_lang", []):
        label = agg.get("label", "?")
        if int(agg.get("scored", 0) or 0) > 0:
            out[f"lang.{label}.recall@5"] = float((agg.get("recall") or {}).get("5", 0.0))
    return out


def scored_counts(report: dict) -> dict[str, int]:
    """每一组的计分题数 —— 容差按它算。

    顺带以 `total:` 前缀记一份**总题数**：抽取侧指标（gold_missing /
    extraction_leak）的分母是 total 而不是 scored ——
    gold 缺失的题恰恰是被排除在 scored 之外的那些，用 scored 当分母
    会在「全部缺失」时得到 0，容差算出来是 floor，等于没守。
    """
    out: dict[str, int] = {}
    for agg in report.get("by_kind", []):
        label = agg.get("label", "?")
        out[f"kind.{label}"] = int(agg.get("scored", 0) or 0)
        out[f"total:kind.{label}"] = int(agg.get("total", 0) or 0)
    for agg in report.get("by_lang", []):
        out[f"lang.{agg.get('label', '?')}"] = int(agg.get("scored", 0) or 0)
    return out


def tolerance_for(metric: str, counts: dict[str, int]) -> float:
    if metric.endswith(".forbidden@5"):
        return FORBIDDEN_TOLERANCE
    if metric.endswith((".gold_missing", ".extraction_leak")):
        # 与 recall 同一套「掉一道放过、掉两道变红」的算法，但分母是 total
        # 而不是 scored —— gold 缺失的题恰恰是 scored 之外的那些。
        n = counts.get("total:" + metric.rsplit(".", 1)[0], 0)
        return PER_KIND_FLOOR if n <= 0 else max(PER_KIND_FLOOR, PER_KIND_QUESTIONS / n)
    if metric.startswith("overall."):
        return OVERALL_TOLERANCE
    # kind.中文语义.recall@5 → kind.中文语义
    prefix = metric.rsplit(".", 1)[0]
    n = counts.get(prefix, 0)
    if n <= 0:
        return PER_KIND_FLOOR
    return max(PER_KIND_FLOOR, PER_KIND_QUESTIONS / n)


# 越低越好的指标后缀。方向判错的后果是**门反着守** —— 退化被当成改进放行。
LOWER_IS_BETTER = (".forbidden@5", ".gold_missing", ".extraction_leak")


def higher_is_better(metric: str) -> bool:
    return not metric.endswith(LOWER_IS_BETTER)


def load(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        sys.exit(f"找不到 {path}")
    except json.JSONDecodeError as e:
        sys.exit(f"{path} 不是合法 JSON：{e}")


def run_info(report: dict) -> str:
    r = report.get("run", {})
    return (
        f"mode={r.get('mode')} embedder={r.get('embedding_model')} "
        f"abstain={r.get('abstain_below')} recency={r.get('recency_mode')} "
        f"episodes={r.get('episode_channel')} peak_bonus={r.get('peak_bonus')} "
        f"semantic_floor={r.get('semantic_floor')}"
    )


def cmd_verify_baseline(args) -> int:
    """基线文件自检。

    基线是回归门的全部依据，它自己损坏 / 缺字段的话，门会**静静地失效** ——
    最危险的是缺 `run.embedding_model`：那样 check 就拦不住「拿 hash 后端的
    分数去撞真实后端的基线」，而两者差 0.016，很容易看起来「只是略降」。
    """
    bad = 0
    for p in args.baselines:
        path = Path(p)
        try:
            d = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as e:
            print(f"✗ {path}: 读不了或不是合法 JSON：{e}", file=sys.stderr)
            bad += 1
            continue
        problems = []
        if not d.get("metrics"):
            problems.append("没有 metrics")
        if not (d.get("run") or {}).get("embedding_model"):
            problems.append("run.embedding_model 缺失 —— 换后端时门就拦不住了")
        if not d.get("scored"):
            problems.append("没有 scored（逐题型容差算不出来，会全部退到下限）")
        if problems:
            print(f"✗ {path}: {'；'.join(problems)}", file=sys.stderr)
            bad += 1
        else:
            print(
                f"✓ {path}: {len(d['metrics'])} 项指标，"
                f"后端 {d['run']['embedding_model']}，"
                f"overall.recall@5 = {d['metrics'].get('overall.recall@5', float('nan')):.4f}"
            )
    return 1 if bad else 0


def cmd_show(args) -> int:
    report = load(Path(args.report))
    print(f"# {run_info(report)}")
    for k, v in sorted(flatten(report).items()):
        print(f"{k:<38} {v:.4f}")
    return 0


def cmd_bless(args) -> int:
    report = load(Path(args.report))
    metrics = flatten(report)
    payload = {
        "_comment": (
            "检索评测基线。由 scripts/evals-gate.py bless 生成。"
            "调参定案后才更新，且必须在 PR 里说明为什么这些数字动了。"
        ),
        "run": report.get("run", {}),
        "suite": report.get("suite", {}),
        "scored": scored_counts(report),
        "metrics": metrics,
    }
    out = Path(args.baseline)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"已写入基线 {out}（{len(metrics)} 项指标）")
    return 0


def cmd_check(args) -> int:
    report = load(Path(args.report))
    base = load(Path(args.baseline))

    cur = flatten(report)
    old = base.get("metrics", {})
    counts = scored_counts(report) or base.get("scored", {})

    # 换了 embedding 后端，两份基线根本不是同一个坐标系 —— 直接拦下来，
    # 比让它「碰巧过了」危险得多
    base_model = (base.get("run") or {}).get("embedding_model")
    cur_model = (report.get("run") or {}).get("embedding_model")
    if base_model and cur_model and base_model != cur_model:
        print(
            f"::error::embedding 后端与基线不一致：基线 {base_model}，本次 {cur_model}。\n"
            f"两者的分数不可比 —— 换后端必须换对应的基线文件。",
            file=sys.stderr,
        )
        return 1

    print(f"基线  {run_info({'run': base.get('run', {})})}")
    print(f"本次  {run_info(report)}")
    print()
    header = f"{'指标':<34}{'基线':>9}{'本次':>9}{'差':>9}{'容差':>8}  判定"
    print(header)
    print("-" * len(header.encode("utf-8").decode("utf-8")) if False else "-" * 78)

    regressions: list[str] = []
    improvements: list[str] = []
    missing: list[str] = []

    for metric in sorted(set(old) | set(cur)):
        if metric not in cur:
            # 基线里有、这次没有 —— 题型被删了或报告结构变了，必须有人看一眼
            missing.append(metric)
            print(f"{metric:<34}{old[metric]:>9.4f}{'—':>9}{'—':>9}{'—':>8}  ⚠ 本次缺失")
            continue
        if metric not in old:
            print(f"{metric:<34}{'—':>9}{cur[metric]:>9.4f}{'—':>9}{'—':>8}  ＋ 基线里没有")
            continue

        tol = tolerance_for(metric, counts)
        delta = cur[metric] - old[metric]
        if not higher_is_better(metric):
            delta = -delta  # 误召率：降了才算好，统一成「delta > 0 是变好」

        if delta < -tol:
            regressions.append(f"{metric}: {old[metric]:.4f} → {cur[metric]:.4f}（容差 {tol:.3f}）")
            verdict = "✗ 回归"
        elif delta > tol:
            improvements.append(f"{metric}: {old[metric]:.4f} → {cur[metric]:.4f}")
            verdict = "↑ 提升"
        else:
            verdict = "· 持平"

        raw = cur[metric] - old[metric]
        print(f"{metric:<34}{old[metric]:>9.4f}{cur[metric]:>9.4f}{raw:>+9.4f}{tol:>8.3f}  {verdict}")

    print()
    if improvements:
        print(f"提升 {len(improvements)} 项：")
        for i in improvements:
            print(f"  ↑ {i}")
        print("  → 确认是有意为之后，跑一次 `evals-gate.py bless` 把基线抬上去，")
        print("    否则门会一直按旧数字放行。")
        print()

    if missing:
        print(f"::warning::基线里有但本次报告缺失 {len(missing)} 项：{', '.join(missing)}")

    if regressions:
        print(f"::error::检索回归门未过，{len(regressions)} 项跌破容差：")
        for r in regressions:
            print(f"  ✗ {r}")
        print()
        print("这不是随机波动 —— seed 模式是确定性的，同样的代码跑一百遍结果一样。")
        print("要么是改动确实让检索变差了，要么是有意的取舍（那就在 PR 里说明并 bless）。")
        return 1

    print(f"回归门通过：{len(cur)} 项指标全部在容差内。")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description="Cortex 检索评测回归门")
    sub = p.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("check", help="与基线逐题型比对")
    c.add_argument("--report", required=True)
    c.add_argument("--baseline", required=True)
    c.set_defaults(func=cmd_check)

    b = sub.add_parser("bless", help="把本次结果写成新基线")
    b.add_argument("--report", required=True)
    b.add_argument("--baseline", required=True)
    b.set_defaults(func=cmd_bless)

    s = sub.add_parser("show", help="打印扁平指标")
    s.add_argument("--report", required=True)
    s.set_defaults(func=cmd_show)

    v = sub.add_parser("verify-baseline", help="检查基线文件本身是否完好")
    v.add_argument("baselines", nargs="+")
    v.set_defaults(func=cmd_verify_baseline)

    args = p.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
