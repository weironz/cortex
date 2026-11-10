"""Cargo 产物根的纯函数及私有临时目录回归；不运行 Cargo 或任何产物。

运行：python -X utf8 -B scripts/test-cargo-target-dir.py -v
"""

import copy
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest import mock


SOURCE = Path(__file__).with_name("cargo-target-dir.py")
TARGET = "x86_64-pc-windows-msvc"


def load_resolver():
    module = types.ModuleType("cargo_target_dir_under_test")
    module.__file__ = str(SOURCE)
    # 在内存编译，避免生成共享工作区 __pycache__ 或执行脚本入口。
    with mock.patch.object(subprocess, "run", side_effect=AssertionError("导入禁止外部命令")), \
            mock.patch.object(subprocess, "Popen", side_effect=AssertionError("导入禁止启动进程")):
        exec(compile(SOURCE.read_bytes(), str(SOURCE), "exec"), module.__dict__)
    return module


RESOLVER = load_resolver()


class IsolatedTests(unittest.TestCase):
    def setUp(self):
        self.blocked = []
        for owner, name in ((subprocess, "run"), (subprocess, "Popen"), (os, "system")):
            patcher = mock.patch.object(owner, name, side_effect=AssertionError("测试禁止 Cargo、构建、GUI 和 driver"))
            self.blocked.append(patcher.start())
            self.addCleanup(patcher.stop)

    def tearDown(self):
        for blocked in self.blocked:
            self.assertEqual(blocked.call_count, 0, "回归测试不得尝试执行外部命令或二进制")


class ResolveTargetDirectoryTests(IsolatedTests):
    def test_accepts_windows_backslashes_and_drive_slashes(self):
        for value, expected in (
            (r"C:\cargo\target", "C:/cargo/target"),
            ("C:/cargo/target", "C:/cargo/target"),
            (r"d:\build\target", "d:/build/target"),
            ("D:/build/target", "D:/build/target"),
        ):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), expected,
                                 "Windows 路径必须保留盘符并转换为 Bash 可拼接的斜杠")

    def test_accepts_unc_paths_without_losing_server_or_share(self):
        for value in (r"\\server\share\target", "//server/share/target", r"\\server\build share\new target"):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), value.replace("\\", "/"),
                                 "UNC 路径必须保留双斜杠、服务器和共享名")

    def test_accepts_posix_absolute_paths(self):
        for value in ("/tmp/cargo-target", "/var/build/target", "/目录/构建产物"):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), value,
                                 "POSIX 绝对路径不得被改成默认 target")

    def test_preserves_spaces_inside_absolute_paths(self):
        for value, expected in (
            (r"C:\build artifacts\new target", "C:/build artifacts/new target"),
            ("C:/build artifacts/new target", "C:/build artifacts/new target"),
            ("/tmp/build artifacts/new target", "/tmp/build artifacts/new target"),
            ("//server/build share/new target", "//server/build share/new target"),
        ):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), expected,
                                 "路径内部空格必须保真，不能拆词、删空格或回落旧路径")

    def test_trims_trailing_separators_without_changing_directory(self):
        for value, expected in (
            ("C:/cargo/target///", "C:/cargo/target"),
            ("C:\\cargo\\target\\\\", "C:/cargo/target"),
            ("/tmp/new target///", "/tmp/new target"),
            ("//server/share/target/", "//server/share/target"),
        ):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), expected,
                                 "非根路径的尾分隔符应移除，不能改变选中的产物根")

    def test_preserves_windows_drive_roots_as_absolute_paths(self):
        for value, expected in (("C:/", "C:/"), ("C:\\", "C:/"), ("d:////", "d:/"), ("Z:\\\\", "Z:/")):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), expected,
                                 "Windows 盘根必须保留斜杠，不能变成 drive-relative C:")

    def test_preserves_posix_and_unc_share_roots(self):
        for value, expected in (("/", "/"), ("////", "/"), ("//server/share/", "//server/share")):
            with self.subTest(path=value):
                self.assertEqual(RESOLVER.resolve_target_directory({"target_directory": value}), expected,
                                 "根目录去尾斜杠后仍必须指向原来的绝对位置")

    def test_requires_standard_target_directory_key(self):
        for metadata in ({}, {"target_dir": "C:/old-target"}, {"workspace_root": "D:/checkout"}):
            with self.subTest(metadata=metadata):
                with self.assertRaises(ValueError, msg="缺少 Cargo 标准 target_directory 字段时必须拒绝，不能猜目录"):
                    RESOLVER.resolve_target_directory(metadata)

    def test_rejects_non_object_metadata(self):
        for metadata in (None, [], ["C:/target"], (), "C:/target", 0, True, False, b"{}"):
            with self.subTest(metadata=metadata):
                with self.assertRaises(ValueError, msg="Cargo metadata 必须是对象，不能把其他 JSON 类型当目录"):
                    RESOLVER.resolve_target_directory(metadata)

    def test_rejects_non_string_or_empty_target_directory(self):
        for value in (None, 0, 123, True, False, 1.5, [], {}, Path("/target"), "", " "):
            with self.subTest(value=value):
                with self.assertRaises(ValueError, msg="产物根必须是非空绝对路径字符串，不能隐式转换或兜底"):
                    RESOLVER.resolve_target_directory({"target_directory": value})

    def test_rejects_relative_and_drive_relative_paths(self):
        for value in ("target", "./target", "../target", r"target\release", "C:target", "D:", "C:",
                      "~/target", "$HOME/target", "%TEMP%/target", " C:/target"):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg=f"相对路径或 drive-relative 路径必须拒绝：{value!r}"):
                    RESOLVER.resolve_target_directory({"target_directory": value})

    def test_rejects_windows_drive_root_relative_backslashes(self):
        for value in (r"\target", r"\build\target"):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg="缺少盘符的 Windows 单反斜杠路径不能冒充 POSIX 绝对路径"):
                    RESOLVER.resolve_target_directory({"target_directory": value})

    def test_rejects_ascii_control_characters(self):
        for control in [chr(number) for number in range(32)] + ["\x7f"]:
            for prefix in ("C:/new", "/tmp/new"):
                value = prefix + control + "target"
                with self.subTest(path=value):
                    with self.assertRaises(ValueError, msg=f"路径中不能包含 ASCII 控制符：{value!r}"):
                        RESOLVER.resolve_target_directory({"target_directory": value})

    def test_reads_only_cargo_metadata_target_and_preserves_input(self):
        metadata = {
            "target_directory": "C:/new target", "workspace_root": "D:/old checkout", "version": 1,
            "packages": [{"name": "cortex-local", "version": "0.1.45"}], "workspace_members": [],
        }
        before = copy.deepcopy(metadata)
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": "D:/old target", "CARGO_BUILD_TARGET": "aarch64-pc-windows-msvc"}):
            actual = RESOLVER.resolve_target_directory(metadata)
        self.assertEqual(actual, "C:/new target", "helper 只能信 Cargo metadata，不能再次从旧环境或版本号猜测")
        self.assertEqual(metadata, before, "解析不得改写 Cargo metadata")


class TargetSelectionTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        temporary = tempfile.TemporaryDirectory(prefix="cortex target selection test ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.new_target = self.root / "new target"
        self.old_target = self.root / "old target"
        self.new_bytes = b"cortex-local 0.1.45\nsynthetic-new-build\n"
        self.old_bytes = b"cortex-local 0.1.45\nsynthetic-old-build\n"
        self.canonical = Path(TARGET) / "release" / "cortex-local.exe"
        self.decoys = [
            self.old_target / self.canonical,
            self.old_target / "release/cortex-local.exe",
            self.new_target / "release/cortex-local.exe",
            self.new_target / "aarch64-pc-windows-msvc/release/cortex-local.exe",
        ]
        for path in self.decoys:
            self.write_fixture(path, self.old_bytes)

    @staticmethod
    def write_fixture(path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        # 这些 .exe 仅是文本夹具；绝不运行 --version 或任何二进制。
        path.write_bytes(value)

    def selected_path(self, metadata):
        # 仅核对 helper 的根与既定三元组的组合，不调用真实打包脚本。
        return Path(RESOLVER.resolve_target_directory(metadata)) / self.canonical

    def test_selects_new_canonical_artifact_when_all_versions_match(self):
        new_artifact = self.new_target / self.canonical
        self.write_fixture(new_artifact, self.new_bytes)
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.old_target), "CARGO_BUILD_TARGET": "aarch64-pc-windows-msvc"}):
            selected = self.selected_path({"target_directory": str(self.new_target) + os.sep})
        self.assertEqual(selected, new_artifact, "必须从 metadata 的新根选取固定 x86_64 三元组产物")
        self.assertEqual(selected.read_bytes(), self.new_bytes, "相同版本不能成为选择旧构建的理由")
        for decoy in self.decoys:
            with self.subTest(decoy=decoy):
                self.assertEqual(decoy.read_bytes().splitlines()[0], self.new_bytes.splitlines()[0], "新旧夹具必须确实同版本")
                self.assertNotEqual(selected, decoy, "不得选旧根、默认 release 或其他架构的同版本旧文件")
                self.assertEqual(decoy.read_bytes(), self.old_bytes, "目录选择不能移动或改写旧产物")

    def test_missing_new_canonical_artifact_does_not_fall_back(self):
        selected = self.selected_path({"target_directory": self.new_target.as_posix()})
        self.assertEqual(selected, self.new_target / self.canonical, "缺少新产物时仍只能检查 canonical 路径")
        with self.assertRaises(FileNotFoundError, msg="canonical 缺失必须失败，不能取默认 release 或另一架构旧文件"):
            selected.read_bytes()
        self.assertTrue(all(path.is_file() for path in self.decoys), "旧文件必须仍然存在，才能证明没有使用 fallback")

    def test_invalid_metadata_cannot_be_rescued_by_existing_old_artifacts(self):
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.old_target)}):
            with self.assertRaises(ValueError, msg="metadata 缺键时即使旧目录同版本文件存在也必须拒绝"):
                self.selected_path({})
        self.assertTrue(all(path.is_file() for path in self.decoys), "拒绝解析不能删除或替换已有夹具")


if __name__ == "__main__":
    unittest.main()
