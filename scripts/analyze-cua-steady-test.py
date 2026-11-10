"""长测分析器的纯内存证据回归；合成 PASS 不代表任何真实长测通过。

运行：python -X utf8 -B scripts/analyze-cua-steady-test.py -v
"""

import copy
import hashlib
from pathlib import Path
import subprocess
import types
import unittest
from unittest import mock


SOURCE = Path(__file__).with_name("analyze-cua-steady.py")
HOUR_MS = 3600000
MINIMUM_MS = 4 * HOUR_MS
MISSING = object()


def load_analyzer():
    module = types.ModuleType("analyze_cua_steady_under_test")
    module.__file__ = str(SOURCE)
    # 不生成工作区缓存，也不让导入意外执行 GUI、外部命令或主入口。
    with mock.patch.object(subprocess, "run", side_effect=AssertionError("导入禁止运行外部命令")), \
            mock.patch.object(subprocess, "Popen", side_effect=AssertionError("导入禁止启动进程")):
        exec(compile(SOURCE.read_bytes(), str(SOURCE), "exec"), module.__dict__)
    return module


ANALYZER = load_analyzer()


def sha(value):
    return hashlib.sha256(value).hexdigest()


def synthetic_evidence():
    # 字段结构来自 harness/owner 契约，时间、PID、指纹和计数全部独立合成。
    run_id = "0123456789abcdef" * 2
    driver_pid, fixture_pid, harness_pid = 70001, 70002, 70003
    points = [("after_warmup", 0, 0)] + [
        ("observations", hour * HOUR_MS, hour * 3600) for hour in range(1, 5)
    ] + [("before_end_session", MINIMUM_MS, 14400)] + [
        ("after_end_session", MINIMUM_MS + delay * 1000, 14400) for delay in range(1, 4)
    ]
    samples = []
    for sequence, (phase, steady, observations) in enumerate(points, 1):
        ended = 73 if phase == "after_end_session" else min(72, observations // 200)
        samples.append({
            "sequence": sequence, "phase": phase, "elapsed_ms": 5000 + steady,
            "steady_elapsed_ms": steady, "observations_completed": observations,
            "warmup_completed": 25, "session_ordinal": min(73, 1 + observations // 200),
            "sessions_ended": ended,
            "metrics": {
                "driver": {"pid": driver_pid, "private_bytes": 10485760, "handles": 120, "threads": 8},
                "fixture": {"pid": fixture_pid, "private_bytes": 2097152, "handles": 60, "threads": 2},
            },
        })
    summary = {
        "schema_version": 2, "scope": "driver_readonly_fixture_candidate_steady",
        "status": "PASS", "phase": "completed", "run_name": "synthetic-only",
        "harness_pid": harness_pid, "owner_run_id": run_id,
        "output_directory": "C:/synthetic-cua-evidence/run",
        "investigation_only": True, "product_signature_gate_passed": False,
        "memory_steady_state_asserted": False, "failure_code": None, "cleanup_failure_code": None,
        "plan": {
            "duration_ms": MINIMUM_MS, "warmup": 25, "session_ms": 600000,
            "session_observations": 200, "max_sessions": 128, "interval_ms": 1000,
            "call_ms": 20000, "sample_ms": HOUR_MS, "setup_ms": 360000, "cleanup_ms": 120000,
            "post_end_samples": 3, "post_end_interval_ms": 1000, "manifest_seconds": 14940,
        },
        "warmup_completed": 25, "observations_completed": 14400,
        "sessions_attempted": 73, "sessions_started": 73, "sessions_ended": 73,
        "rotations": 72, "session_end_attempts": 73, "max_seen_snapshots": 200,
        "post_end_samples": 3, "cli_attempts": 14571, "cli_exit_codes": 14571, "cli_zero_exits": 14571,
        "steady_elapsed_ms": MINIMUM_MS, "samples_written": len(samples),
        "elapsed_ms": samples[-1]["elapsed_ms"], "daemon_pid": driver_pid, "fixture_pid": fixture_pid,
        "candidate_sha256": sha(b"synthetic candidate only"), "fixture_sha256": sha(b"synthetic fixture only"),
        "daemon_cleanup_ok": True, "fixture_cleanup_ok": True, "fixture_copy_removed": True,
    }
    sources = {
        "C:/synthetic-cua-evidence/" + name: sha(("synthetic source: " + name).encode("utf-8"))
        for name in ("owned-job.cs", "run-owned-steady.ps1", "cua-readonly-steady.ps1", "cua-readonly-ab.ps1")
    }
    owner = {
        "schema": 2, "scope": "owned_cua_steady_wrapper", "run_id": run_id, "child_pid": harness_pid,
        "child_exit_code": 0, "original_job_drained": True, "source_changed": False,
        "sources": sources, "helper_sha256": sources["C:/synthetic-cua-evidence/owned-job.cs"],
        "log": "C:/synthetic-cua-evidence/child.log",
    }
    return {"samples": samples, "summary": summary, "owner": owner, "sha": summary["candidate_sha256"]}


def analyze_evidence(evidence, **kwargs):
    return ANALYZER.analyze(evidence["samples"], evidence["summary"], evidence["owner"], evidence["sha"], **kwargs)


class IsolatedTests(unittest.TestCase):
    def setUp(self):
        self.blocked = []
        for owner, name in ((subprocess, "run"), (subprocess, "Popen"), (ANALYZER, "main")):
            patcher = mock.patch.object(owner, name, side_effect=AssertionError("单元测试禁止启动真实进程或主入口"))
            self.blocked.append(patcher.start())
            self.addCleanup(patcher.stop)

    def tearDown(self):
        for blocked in self.blocked:
            self.assertEqual(blocked.call_count, 0, "分析回归不得调用进程、Cua、GUI 或主入口")


class AnalyzeTests(IsolatedTests):
    def assert_rejected(self, evidence, code):
        result = analyze_evidence(evidence)
        self.assertFalse(result["completedRunVerified"], "证据不完整或身份不符时不能宣称运行完成")
        self.assertIn(code, result["integrityErrors"], "必须保留可定位的拒绝原因")
        self.assertFalse(result["memorySteadyStateProven"], "拒绝结果不能宣称内存稳态通过")
        self.assertFalse(result["productionAcceptance"], "拒绝结果不能宣称生产验收通过")

    def reject_field(self, section, field, value, code=None):
        evidence = synthetic_evidence()
        target = evidence["summary"]["plan"] if section == "plan" else evidence[section]
        if value is MISSING:
            target.pop(field, None)
        else:
            target[field] = value
        if code is None:
            with self.assertRaises(ValueError, msg=f"缺失或非严格整数的证据必须拒绝：{section}.{field}"):
                analyze_evidence(evidence)
        else:
            self.assert_rejected(evidence, code)

    def test_complete_synthetic_evidence_verifies_integrity_only(self):
        evidence = synthetic_evidence()
        before = copy.deepcopy(evidence)
        result = analyze_evidence(evidence)
        self.assertTrue(result["completedRunVerified"], "完整合成证据必须能通过运行完整性门禁")
        self.assertEqual(result["integrityErrors"], [], "有效夹具不应夹带完整性错误")
        self.assertFalse(result["previewOnly"], "默认分析应是终态完整性校验")
        self.assertFalse(result["memorySteadyStateProven"], "合成完整性通过不代表真实内存稳态通过")
        self.assertFalse(result["productionAcceptance"], "合成完整性通过不代表产品生产验收")
        self.assertEqual(result["requiredSteadyMs"], MINIMUM_MS, "默认最短时长必须保持四小时")
        self.assertEqual(evidence, before, "分析不得改写输入证据")

    def test_running_fail_and_missing_status_never_complete(self):
        for value in ("RUNNING", "FAIL", "pass", MISSING):
            with self.subTest(status=value):
                self.reject_field("summary", "status", value, "not_pass")

    def test_preview_never_completes_even_with_valid_pass(self):
        for status in ("PASS", "RUNNING", "FAIL"):
            with self.subTest(status=status):
                evidence = synthetic_evidence()
                evidence["summary"]["status"] = status
                result = analyze_evidence(evidence, preview=True)
                self.assertTrue(result["previewOnly"], "预览必须明确标注为预览")
                self.assertFalse(result["completedRunVerified"], "预览即使拿到完整 PASS 也不能成为完成证据")
                self.assertFalse(result["memorySteadyStateProven"], "预览不能证明内存稳态")

    def test_summary_scope_and_candidate_hash_are_bound(self):
        for field, code in (("scope", "scope_mismatch"), ("candidate_sha256", "driver_hash_mismatch")):
            for value in ("other-evidence", MISSING):
                with self.subTest(field=field, value=value):
                    self.reject_field("summary", field, value, code)

    def test_reported_failure_rejects_even_with_pass_status(self):
        for field in ("failure_code", "cleanup_failure_code"):
            with self.subTest(field=field):
                self.reject_field("summary", field, "SYNTHETIC_FAILURE", "reported_failure")

    def test_missing_original_owner_report_rejects(self):
        evidence = synthetic_evidence()
        evidence["owner"] = None
        self.assert_rejected(evidence, "missing_original_job_report")

    def test_owner_scope_cannot_be_borrowed_from_build_or_other_run(self):
        for value in ("owned_cua_build_wrapper", "another_scope", MISSING):
            with self.subTest(scope=value):
                self.reject_field("owner", "scope", value, "wrong_owner_scope")

    def test_owner_run_id_must_match_and_be_present_in_both_reports(self):
        for section, field in (("summary", "owner_run_id"), ("owner", "run_id")):
            for value in ("f" * 32, "short", 123, MISSING):
                with self.subTest(section=section, value=value):
                    self.reject_field(section, field, value, "owner_run_binding_missing_or_mismatched")

    def test_child_and_harness_pid_must_match_and_be_present(self):
        for section, field in (("summary", "harness_pid"), ("owner", "child_pid")):
            for value in (70004, "70003", True, 0, MISSING):
                with self.subTest(section=section, value=value):
                    self.reject_field(section, field, value, "owner_process_binding_missing_or_mismatched")

    def test_owner_child_pid_requires_integer_identity(self):
        for harness, child in ((70003, 70003.0), (1, True)):
            with self.subTest(harness=harness, child=child):
                evidence = synthetic_evidence()
                evidence["summary"]["harness_pid"] = harness
                evidence["owner"]["child_pid"] = child
                self.assert_rejected(evidence, "owner_process_binding_missing_or_mismatched")

    def test_original_job_must_be_explicitly_drained(self):
        for value in (False, None, 1, "true", MISSING):
            with self.subTest(value=value):
                self.reject_field("owner", "original_job_drained", value, "job_not_drained")

    def test_sources_must_be_explicitly_unchanged(self):
        for value in (True, None, 0, "false", MISSING):
            with self.subTest(value=value):
                self.reject_field("owner", "source_changed", value, "source_changed_or_unknown")

    def test_owner_child_exit_must_be_integer_zero(self):
        for value in (1, -1, False, 0.0, "0", MISSING):
            with self.subTest(value=value):
                self.reject_field("owner", "child_exit_code", value, "owner_child_exit")

    def test_source_fingerprints_require_four_path_hash_pairs(self):
        sources = synthetic_evidence()["owner"]["sources"]
        malformed = [None, {}, [], True, list(sources), dict(list(sources.items())[:3])]
        for key, value in (("", "a" * 64), (7, "a" * 64), ("bad", "g" * 64),
                           ("bad", "a" * 63), ("bad", "A" * 64), ("bad", True)):
            malformed.append({**sources, key: value})
        for value in malformed + [MISSING]:
            with self.subTest(sources=value):
                self.reject_field("owner", "sources", value, "missing_source_fingerprints")

    def test_plan_must_reach_default_minimum_duration(self):
        self.reject_field("plan", "duration_ms", MINIMUM_MS - 1, "plan_too_short")

    def test_summary_duration_must_reach_declared_duration(self):
        self.reject_field("summary", "steady_elapsed_ms", MINIMUM_MS - 1, "duration_incomplete")

    def test_missing_plan_or_declared_duration_cannot_complete(self):
        for field in ("plan", "duration_ms"):
            evidence = synthetic_evidence()
            target = evidence["summary"] if field == "plan" else evidence["summary"]["plan"]
            del target[field]
            with self.subTest(missing=field):
                with self.assertRaises((ValueError, KeyError), msg="缺失计划或计划时长时不能补默认值来宣称完成"):
                    analyze_evidence(evidence)

    def test_custom_minimum_does_not_silently_override_default(self):
        evidence = synthetic_evidence()
        result = analyze_evidence(evidence, minimum_ms=MINIMUM_MS + 1)
        self.assertFalse(result["completedRunVerified"], "调用方提高最短时长时必须执行新门禁")
        self.assertIn("plan_too_short", result["integrityErrors"], "计划未达到调用方门槛必须说明原因")

    def test_sample_count_must_equal_actual_rows(self):
        count = len(synthetic_evidence()["samples"])
        for value in (count - 1, count + 1):
            with self.subTest(count=value):
                self.reject_field("summary", "samples_written", value, "sample_count_mismatch")

    def test_final_observations_must_equal_summary_and_be_positive(self):
        for value in (0, 14399, 14401):
            with self.subTest(count=value):
                self.reject_field("summary", "observations_completed", value, "missing_final_observations")

    def test_sessions_must_have_started_and_all_ended(self):
        self.reject_field("summary", "sessions_started", 0, "missing_started_session")
        self.reject_field("summary", "sessions_ended", 72, "sessions_not_ended")
        for field in ("sessions_started", "sessions_ended"):
            with self.subTest(missing=field):
                self.reject_field("summary", field, MISSING)

    def test_cli_evidence_cannot_be_missing_or_nonzero(self):
        for field in ("cli_attempts", "cli_exit_codes", "cli_zero_exits"):
            with self.subTest(missing=field):
                self.reject_field("summary", field, MISSING)
        for field in ("cli_exit_codes", "cli_zero_exits"):
            with self.subTest(nonzero=field):
                self.reject_field("summary", field, 14570, "cli_not_all_zero")

    def test_cli_counters_reject_boolean_integer_equivalence(self):
        for field in ("cli_attempts", "cli_exit_codes", "cli_zero_exits"):
            for value in (True, False, 1.0, "1"):
                with self.subTest(field=field, value=value):
                    evidence = synthetic_evidence()
                    for key in ("cli_attempts", "cli_exit_codes", "cli_zero_exits"):
                        evidence["summary"][key] = 1
                    evidence["summary"][field] = value
                    with self.assertRaises(ValueError, msg="CLI 计数必须为严格整数，不能借 True 等价于 1 蒙混过关"):
                        analyze_evidence(evidence)

    def test_warmup_count_must_match_plan(self):
        for value in (0, 24, 26):
            with self.subTest(count=value):
                self.reject_field("summary", "warmup_completed", value, "warmup_incomplete")
        for section, field in (("summary", "warmup_completed"), ("plan", "warmup")):
            for value in (True, "25", MISSING):
                with self.subTest(section=section, value=value):
                    self.reject_field(section, field, value)

    def test_cleanup_requires_all_explicit_true_fields(self):
        for field in ("daemon_cleanup_ok", "fixture_cleanup_ok", "fixture_copy_removed"):
            for value in (False, 1, "true", MISSING):
                with self.subTest(field=field, value=value):
                    self.reject_field("summary", field, value, field)

    def test_steady_claim_is_never_inherited_from_summary(self):
        for value in (True, 0, MISSING):
            with self.subTest(value=value):
                self.reject_field("summary", "memory_steady_state_asserted", value, "unexpected_steady_assertion")

    def test_warmup_and_full_duration_boundaries_are_required(self):
        for phase, code in (("after_warmup", "missing_warmup_boundary"),
                            ("before_end_session", "missing_full_duration_boundary")):
            with self.subTest(phase=phase):
                evidence = synthetic_evidence()
                for row in evidence["samples"]:
                    if row["phase"] == phase:
                        row["phase"] = "observations"
                self.assert_rejected(evidence, code)

    def test_post_end_boundary_count_must_match_plan_and_summary(self):
        for section in ("summary", "plan"):
            with self.subTest(section=section):
                self.reject_field(section, "post_end_samples", 2, "missing_post_end_samples")
        evidence = synthetic_evidence()
        evidence["samples"][-1]["phase"] = "cleanup"
        self.assert_rejected(evidence, "missing_post_end_samples")

    def test_post_end_counts_reject_boolean_integer_equivalence(self):
        for section in ("summary", "plan"):
            evidence = synthetic_evidence()
            evidence["samples"] = evidence["samples"][:-2]
            evidence["summary"]["samples_written"] = len(evidence["samples"])
            evidence["summary"]["post_end_samples"] = 1
            evidence["summary"]["plan"]["post_end_samples"] = 1
            target = evidence["summary"]["plan"] if section == "plan" else evidence["summary"]
            target["post_end_samples"] = True
            with self.subTest(section=section):
                with self.assertRaises(ValueError, msg="post-end 计数也必须拒绝 True 等价于 1"):
                    analyze_evidence(evidence)

    def test_missing_numeric_evidence_is_rejected(self):
        for section, fields in (
            ("summary", ("daemon_pid", "fixture_pid", "samples_written", "steady_elapsed_ms", "observations_completed", "post_end_samples")),
            ("plan", ("warmup", "post_end_samples")),
        ):
            for field in fields:
                with self.subTest(section=section, field=field):
                    self.reject_field(section, field, MISSING)


class ValidateSamplesTests(IsolatedTests):
    def validate(self, samples):
        summary = synthetic_evidence()["summary"]
        return ANALYZER.validate_samples(samples, summary["daemon_pid"], summary["fixture_pid"])

    def test_accepts_complete_samples_without_mutation(self):
        samples = synthetic_evidence()["samples"]
        before = copy.deepcopy(samples)
        self.assertIsNone(self.validate(samples), "完整采样序列必须可通过身份和单调性校验")
        self.assertEqual(samples, before, "采样校验不能排序或改写原始证据")

    def test_rejects_empty_samples(self):
        with self.assertRaisesRegex(ValueError, "采样为空", msg="没有采样不能生成长测证据"):
            self.validate([])

    def test_rejects_reordered_and_duplicate_sequences(self):
        samples = synthetic_evidence()["samples"]
        for rows in (list(reversed(samples)), samples[:2] + [samples[1]] + samples[2:],
                     [samples[0], samples[2], samples[1]] + samples[3:]):
            with self.subTest(sequences=[row["sequence"] for row in rows]):
                with self.assertRaisesRegex(ValueError, "序号", msg="乱序或重复采样不得自动排序后拼接"):
                    self.validate(rows)

    def test_rejects_time_and_observation_regression(self):
        for field in ("elapsed_ms", "steady_elapsed_ms", "observations_completed"):
            samples = synthetic_evidence()["samples"]
            samples[2][field] = samples[1][field] - 1
            with self.subTest(field=field):
                with self.assertRaisesRegex(ValueError, "倒退", msg="时钟或 observation 回退必须拒绝"):
                    self.validate(samples)

    def test_rejects_driver_or_fixture_pid_switch(self):
        for role in ("driver", "fixture"):
            samples = synthetic_evidence()["samples"]
            samples[2]["metrics"][role]["pid"] += 4
            with self.subTest(role=role):
                with self.assertRaisesRegex(ValueError, "进程身份改变", msg="运行中更换 PID 不能拼接成同进程长测"):
                    self.validate(samples)

    def test_rejects_boolean_sample_counters_and_metrics(self):
        paths = [(name,) for name in ("sequence", "elapsed_ms", "steady_elapsed_ms", "observations_completed")]
        paths += [("metrics", role, name) for role in ("driver", "fixture")
                  for name in ("pid", "private_bytes", "handles", "threads")]
        for path in paths:
            for value in (True, False):
                samples = synthetic_evidence()["samples"]
                target = samples[2]
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                with self.subTest(path=path, value=value):
                    with self.assertRaisesRegex(ValueError, "不是有效整数", msg="采样所有计数和指标必须拒绝布尔整数别名"):
                        self.validate(samples)

    def test_rejects_missing_fractional_string_and_negative_metrics(self):
        for value in (None, "120", 120.0, -1):
            samples = synthetic_evidence()["samples"]
            samples[2]["metrics"]["driver"]["handles"] = value
            with self.subTest(value=value):
                with self.assertRaises(ValueError, msg="缺失、浮点、字符串或负数指标都不是有效计数"):
                    self.validate(samples)

    def test_rejects_missing_sequence_or_pid(self):
        for field in ("sequence", "pid"):
            samples = synthetic_evidence()["samples"]
            target = samples[2] if field == "sequence" else samples[2]["metrics"]["driver"]
            del target[field]
            with self.subTest(field=field):
                with self.assertRaises(ValueError, msg="采样缺失序号或 PID 必须拒绝"):
                    self.validate(samples)


class SeriesMetricsTests(IsolatedTests):
    def metrics(self, points, field="private_bytes", role="driver"):
        samples = [{"steady_elapsed_ms": time, "metrics": {role: {field: value}}} for time, value in points]
        before = copy.deepcopy(samples)
        result = ANALYZER.series_metrics(samples, role, field)
        self.assertEqual(samples, before, "趋势计算不得重排或修改原始采样")
        return result

    def test_constant_series_has_zero_robust_slope(self):
        result = self.metrics([(hour * HOUR_MS, 1234) for hour in range(5)])
        self.assertEqual(result, {
            "points": 5, "slopePoints": 5, "start": 1234, "end": 1234, "min": 1234,
            "max": 1234, "delta": 0, "theilSenUnitsPerHour": 0.0,
        }, "常值序列的变化量和稳健斜率必须为零")

    def test_irregular_linear_series_has_known_per_hour_slope(self):
        result = self.metrics([(0, 100), (HOUR_MS // 2, 110), (3 * HOUR_MS // 2, 130), (3 * HOUR_MS, 160)])
        self.assertAlmostEqual(result["theilSenUnitsPerHour"], 20.0, places=9,
                               msg="不等间隔线性序列应按真实时间得到每小时 20 单位")
        self.assertEqual((result["start"], result["end"], result["delta"], result["min"], result["max"]),
                         (100, 160, 60, 100, 160), "线性序列的端点与范围必须保真")

    def test_handles_and_fixture_role_use_same_units(self):
        result = self.metrics([(0, 30), (HOUR_MS, 26), (2 * HOUR_MS, 22)], field="handles", role="fixture")
        self.assertAlmostEqual(result["theilSenUnitsPerHour"], -4.0, places=9,
                               msg="fixture 句柄下降趋势应得到每小时负四，不能反转符号或单位")

    def test_isolated_outlier_does_not_replace_robust_linear_slope(self):
        points = [(hour * HOUR_MS, 100 + 3 * hour) for hour in range(9)]
        points[4] = (4 * HOUR_MS, 999999)
        result = self.metrics(points)
        self.assertAlmostEqual(result["theilSenUnitsPerHour"], 3.0, places=9,
                               msg="单点尖峰不应把 Theil-Sen 稳健斜率拖离原来的每小时三单位")
        self.assertEqual(result["max"], 999999, "稳健斜率不能通过删掉尖峰来伪造原始指标范围")

    def test_dense_series_preserves_endpoints_and_known_slope(self):
        result = self.metrics([(minute * 60000, 200 + 2 * minute) for minute in range(600)])
        self.assertEqual((result["points"], result["slopePoints"]), (600, 512), "密集序列应记录原点数和斜率抽样点数")
        self.assertEqual((result["start"], result["end"], result["delta"]), (200, 1398, 1198), "抽样必须保留两端原始值")
        self.assertAlmostEqual(result["theilSenUnitsPerHour"], 120.0, places=6,
                               msg="限制斜率抽样数量后仍应保持每小时 120 单位的已知线性斜率")

    def test_empty_or_single_point_cannot_invent_slope(self):
        self.assertIsNone(self.metrics([]), "空序列不能凭空产生趋势")
        result = self.metrics([(0, 100)])
        self.assertIsNone(result["theilSenUnitsPerHour"], "单点不足以计算斜率")
        self.assertEqual(result["delta"], 0, "单点序列的端点差为零")

    def test_equal_timestamps_do_not_divide_by_zero(self):
        result = self.metrics([(0, 100), (0, 100), (HOUR_MS, 120)])
        self.assertAlmostEqual(result["theilSenUnitsPerHour"], 20.0, places=9,
                               msg="重复时间点应跳过零时间差，仍使用其余有效时间差")
        self.assertIsNone(self.metrics([(0, 100), (0, 120)])["theilSenUnitsPerHour"], "全部时间相同不能生成斜率")


if __name__ == "__main__":
    unittest.main()
