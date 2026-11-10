"""离线核对只读长测及采样趋势；预览、失败或趋势数字都不自行证明内存稳态。"""
import argparse
import hashlib
import json
import re
from pathlib import Path
import statistics


def integer(value, label, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError(f"{label} 不是有效整数")
    return value


def validate_samples(samples, driver_pid, fixture_pid):
    if not samples:
        raise ValueError("采样为空")
    previous = {"elapsed_ms": -1, "steady_elapsed_ms": -1, "observations_completed": -1}
    for sequence, sample in enumerate(samples, 1):
        if integer(sample.get("sequence"), "sequence", 1) != sequence:
            raise ValueError("采样序号缺失、重复或乱序")
        for key in previous:
            value = integer(sample.get(key), key)
            if value < previous[key]:
                raise ValueError(f"采样 {key} 倒退")
            previous[key] = value
        for role, pid in (("driver", driver_pid), ("fixture", fixture_pid)):
            metric = sample["metrics"][role]
            if integer(metric.get("pid"), role + " pid", 1) != pid:
                raise ValueError("运行中进程身份改变，不能拼接成同进程长测")
            for key in ("private_bytes", "handles", "threads"):
                integer(metric.get(key), role + key)


def series_metrics(samples, role, field):
    values = [(row["steady_elapsed_ms"] / 3600000, row["metrics"][role][field]) for row in samples]
    if not values:
        return None
    # 正常 30s/4h 不需抽样；对更密集数据限制 O(n²)，保留时间端点，不静默修改原序列。
    indices = sorted({round(index * (len(values) - 1) / min(511, len(values) - 1))
                      for index in range(min(512, len(values)))}) if len(values) > 1 else [0]
    chosen = [values[index] for index in indices]
    slopes = [(second[1] - first[1]) / (second[0] - first[0])
              for index, first in enumerate(chosen) for second in chosen[index+1:] if second[0] > first[0]]
    return {"points": len(values), "slopePoints": len(chosen), "start": values[0][1], "end": values[-1][1],
            "min": min(value for _, value in values), "max": max(value for _, value in values),
            "delta": values[-1][1] - values[0][1],
            "theilSenUnitsPerHour": statistics.median(slopes) if slopes else None}


def analyze(samples, summary, owner, expected_sha, minimum_ms=14400000, preview=False):
    driver_pid = integer(summary.get("daemon_pid"), "daemon_pid", 1)
    fixture_pid = integer(summary.get("fixture_pid"), "fixture_pid", 1)
    validate_samples(samples, driver_pid, fixture_pid)
    errors = []
    require = lambda condition, code: errors.append(code) if not condition else None
    require(summary.get("scope") == "driver_readonly_fixture_candidate_steady", "scope_mismatch")
    require(summary.get("candidate_sha256") == expected_sha, "driver_hash_mismatch")
    require(summary.get("status") == "PASS", "not_pass")
    require(summary.get("failure_code") is None and summary.get("cleanup_failure_code") is None, "reported_failure")
    duration = integer(summary["plan"]["duration_ms"], "plan duration", 1)
    require(duration >= minimum_ms, "plan_too_short")
    require(integer(summary.get("steady_elapsed_ms"), "steady duration") >= duration, "duration_incomplete")
    require(integer(summary.get("samples_written"), "samples_written", 1) == len(samples), "sample_count_mismatch")
    observations = integer(summary.get("observations_completed"), "observations")
    require(observations > 0 and observations == samples[-1]["observations_completed"], "missing_final_observations")
    started = integer(summary.get("sessions_started"), "sessions_started")
    require(started > 0, "missing_started_session")
    require(started == integer(summary.get("sessions_ended"), "sessions_ended"), "sessions_not_ended")
    attempts = integer(summary.get("cli_attempts"), "cli_attempts", 1)
    exits = integer(summary.get("cli_exit_codes"), "cli_exit_codes")
    zeros = integer(summary.get("cli_zero_exits"), "cli_zero_exits")
    require(attempts == exits == zeros, "cli_not_all_zero")
    require(integer(summary.get("warmup_completed"), "warmup_completed") == integer(summary["plan"].get("warmup"), "plan warmup", 1), "warmup_incomplete")
    for field in ("daemon_cleanup_ok", "fixture_cleanup_ok", "fixture_copy_removed"):
        require(summary.get(field) is True, field)
    require(summary.get("memory_steady_state_asserted") is False, "unexpected_steady_assertion")
    require(any(row["phase"] == "after_warmup" for row in samples), "missing_warmup_boundary")
    require(any(row["phase"] == "before_end_session" and row["steady_elapsed_ms"] >= duration for row in samples), "missing_full_duration_boundary")
    post_count = integer(summary.get("post_end_samples"), "post_end_samples")
    post_plan = integer(summary["plan"].get("post_end_samples"), "plan post_end_samples", 1)
    require(sum(row["phase"] == "after_end_session" for row in samples) == post_count == post_plan, "missing_post_end_samples")
    if owner is None:
        require(False, "missing_original_job_report")
    else:
        require(owner.get("scope") == "owned_cua_steady_wrapper", "wrong_owner_scope")
        run_id = summary.get("owner_run_id")
        require(isinstance(run_id, str) and len(run_id) == 32 and owner.get("run_id") == run_id,
                "owner_run_binding_missing_or_mismatched")
        require(type(summary.get("harness_pid")) is int and summary["harness_pid"] > 0
                and type(owner.get("child_pid")) is int
                and owner.get("child_pid") == summary["harness_pid"], "owner_process_binding_missing_or_mismatched")
        require(owner.get("original_job_drained") is True, "job_not_drained")
        require(owner.get("source_changed") is False, "source_changed_or_unknown")
        require(type(owner.get("child_exit_code")) is int and owner["child_exit_code"] == 0, "owner_child_exit")
        sources = owner.get("sources")
        require(isinstance(sources, dict) and len(sources) >= 4
                and all(isinstance(key, str) and key and isinstance(value, str)
                        and re.fullmatch(r"[0-9a-f]{64}", value) for key, value in sources.items()),
                "missing_source_fingerprints")
    periodic = [row for row in samples if row["phase"] in ("after_warmup", "observations")]
    end_time = periodic[-1]["steady_elapsed_ms"] if periodic else 0
    last_half = [row for row in periodic if row["steady_elapsed_ms"] >= end_time / 2]
    gaps = [(right["elapsed_ms"] - left["elapsed_ms"], left["sequence"], right["sequence"])
            for left, right in zip(samples, samples[1:])]
    largest_gap = max(gaps, default=(0, 0, 0))
    return {"schema": 1, "scope": "readonly_candidate_run_integrity_and_descriptive_trends",
            "previewOnly": bool(preview), "completedRunVerified": not preview and not errors,
            "integrityErrors": errors, "memorySteadyStateProven": False, "productionAcceptance": False,
            "observedSteadyMs": summary["steady_elapsed_ms"], "requiredSteadyMs": duration,
            "largestGap": {"ms": largest_gap[0], "fromSequence": largest_gap[1], "toSequence": largest_gap[2]},
            "driver": {field: {"wholeSeries": series_metrics(periodic, "driver", field),
                                "lastHalf": series_metrics(last_half, "driver", field)} for field in ("private_bytes", "handles")},
            "interpretation": "趋势仅描述该工作负载；完整运行不等于内存稳态或全产品通过。"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--owner-report", type=Path)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--minimum-seconds", type=int, default=14400)
    parser.add_argument("--preview", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("不覆盖旧分析证据；请使用新文件名")
    if args.minimum_seconds <= 0 or len(args.expected_sha256) != 64 or any(char not in "0123456789abcdef" for char in args.expected_sha256):
        parser.error("时长或独立摘要格式不合法")
    samples_file = args.run_dir / "samples.jsonl"
    if samples_file.stat().st_size > 16 * 1024 * 1024:
        parser.error("采样超出16MiB分析上界")
    samples_bytes = samples_file.read_bytes()
    samples = [json.loads(line) for line in samples_bytes.decode("utf-8-sig").splitlines() if line]
    summary_path = args.run_dir / ("progress.json" if args.preview else "summary.json")
    summary_bytes = summary_path.read_bytes()
    summary = json.loads(summary_bytes.decode("utf-8-sig"))
    owner = json.loads(args.owner_report.read_text(encoding="utf-8-sig")) if args.owner_report else None
    result = analyze(samples, summary, owner, args.expected_sha256, args.minimum_seconds * 1000, args.preview)
    result["inputs"] = {"samplesSha256": hashlib.sha256(samples_bytes).hexdigest(),
                        "summarySha256": hashlib.sha256(summary_bytes).hexdigest(),
                        "analyzerSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"completedRunVerified": result["completedRunVerified"], "previewOnly": args.preview,
                      "integrityErrors": result["integrityErrors"], "largestGap": result["largestGap"]}, ensure_ascii=False))
    if not args.preview and not result["completedRunVerified"]:
        raise SystemExit(2)


if __name__ == "__main__":
    main()
