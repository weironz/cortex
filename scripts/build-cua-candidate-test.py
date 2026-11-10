"""候选构建器的离线安全回归；只使用内存对象和独立临时目录。

运行：python -B scripts/build-cua-candidate-test.py -v
"""

import hashlib
import os
from pathlib import Path, PurePosixPath
import subprocess
import tempfile
import types
import unittest
from unittest import mock


SOURCE = Path(__file__).with_name("build-cua-candidate.py")


def load_candidate():
    # 不走文件加载器，避免在 Main 的工作区生成 __pycache__。
    module = types.ModuleType("build_cua_candidate_under_test")
    module.__file__ = str(SOURCE)
    with mock.patch.object(subprocess, "run", side_effect=AssertionError("导入禁止启动子进程")), \
            mock.patch.object(subprocess, "Popen", side_effect=AssertionError("导入禁止启动构建")):
        exec(compile(SOURCE.read_bytes(), str(SOURCE), "exec"), module.__dict__)
    return module


CANDIDATE = load_candidate()


def blob_oid(value):
    # Git 的对象身份包含类型与字节长度，不能用裸内容哈希替代。
    header = b"blob " + str(len(value)).encode("ascii") + b"\0"
    return hashlib.sha1(header + value).hexdigest()


def tree_entry(path, value=b"fixture\n", *, mode="100644", kind="blob", oid=None):
    return (
        f"{mode} {kind} {oid or blob_oid(value)}\t".encode("ascii")
        + path.encode("utf-8") + b"\0"
    )


def batch_blob(value, *, oid=None):
    return f"{oid or blob_oid(value)} blob {len(value)}\n".encode("ascii") + value + b"\n"


class IsolatedTests(unittest.TestCase):
    def setUp(self):
        self.blocked = []
        for owner, name in (
            (subprocess, "run"), (subprocess, "Popen"),
            (CANDIDATE, "build"), (CANDIDATE, "main"),
        ):
            patcher = mock.patch.object(
                owner, name, side_effect=AssertionError("安全测试禁止真实命令、构建和主入口"),
            )
            self.blocked.append(patcher.start())
            self.addCleanup(patcher.stop)
        patcher = mock.patch.object(
            CANDIDATE, "command", side_effect=AssertionError("命令必须由内存替身明确响应"),
        )
        self.command = patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        for blocked in self.blocked:
            self.assertEqual(blocked.call_count, 0, "测试不得尝试启动真实命令或构建")


class SafeRelativeTests(IsolatedTests):
    def test_accepts_canonical_relative_paths(self):
        for value in (
            "AGENTS.md", "libs/cua-driver/rust/Cargo.toml", "目录/测试.py",
            ".github/workflows/build.yml", "a/.gitignore", "a/..hidden", "a space/file",
        ):
            with self.subTest(path=value):
                result = CANDIDATE.safe_relative(value)
                self.assertIsInstance(result, PurePosixPath, "安全路径应保留 POSIX 分段语义")
                self.assertEqual(str(result), value, "规范相对路径不应被静默改写")

    def test_rejects_escape_and_noncanonical_paths(self):
        for value in (
            "", ".", "..", "../escape", "a/../../escape", "/absolute", "//server/share",
            "a/..", "a/./file", "./a", "a//b", "a/", "a\\b", "C:/file", "C:relative",
            "a:b", "\\\\server\\share", ".git", ".git/config", "a/.git/config",
        ):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg=f"必须拒绝越界或非规范路径：{value!r}"):
                    CANDIDATE.safe_relative(value)

    def test_rejects_case_insensitive_git_directory(self):
        for value in (".GIT/config", "directory/.GiT/HEAD", "directory/.git/objects/blob"):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg="Windows 的 .GIT 与 .git 同目录，必须拒绝"):
                    CANDIDATE.safe_relative(value)

    def test_rejects_win32_dot_suffixed_git_directory(self):
        with self.assertRaises(ValueError, msg="Win32 会把 .git. 映射为 .git，必须拒绝"):
            CANDIDATE.safe_relative(".git./config")

    def test_rejects_trailing_dots_or_spaces_in_any_component(self):
        for value in (
            "file.", "file ", "file..", "file. ", "file .", "directory./file",
            "directory /file", "safe/directory. /file", "safe/file.", "safe/file ",
            ".git /config", "safe/.GiT. /config",
        ):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg=f"每一层路径都必须拒绝 Win32 尾点或空格别名：{value!r}"):
                    CANDIDATE.safe_relative(value)

    def test_rejects_windows_reserved_devices_with_extensions(self):
        devices = ("CON", "PRN", "AUX", "NUL") + tuple(
            prefix + number for prefix in ("COM", "LPT") for number in "123456789¹²³"
        )
        for device in devices:
            for name in (device, device.lower() + ".txt", device + ".more.txt"):
                for value in (name, "safe/" + name + "/file"):
                    with self.subTest(path=value):
                        with self.assertRaises(ValueError, msg=f"设备名带扩展名或位于目录中也必须拒绝：{value!r}"):
                            CANDIDATE.safe_relative(value)

    def test_rejects_windows_invalid_characters(self):
        invalid = '<>:"|?*' + "".join(chr(number) for number in range(32))
        for char in invalid:
            for value in (f"bad{char}name.txt", f"safe/bad{char}directory/file"):
                with self.subTest(path=value):
                    with self.assertRaises(ValueError, msg=f"文件名和目录名都必须拒绝 Windows 非法字符：{value!r}"):
                        CANDIDATE.safe_relative(value)

    def test_accepts_names_near_reserved_devices(self):
        for value in (
            "CONsole.txt", "AUXiliary/file", "COM0", "COM10.txt", "LPT0", "LPT10",
            "prefix.CON", "safe/nul-device.txt", "safe/file.name", "safe/a b.txt",
        ):
            with self.subTest(path=value):
                self.assertEqual(str(CANDIDATE.safe_relative(value)), value,
                                 "合法的相似名称、内部空格和扩展名不能被设备名校验误伤")


class ParseTreeTests(IsolatedTests):
    def test_preserves_regular_executable_and_utf8_paths(self):
        value = b"same blob\n"
        data = tree_entry("a.txt", value) + tree_entry("目录/a b.sh", value, mode="100755")
        self.assertEqual(CANDIDATE.parse_tree(data), [
            {"mode": "100644", "oid": blob_oid(value), "path": "a.txt"},
            {"mode": "100755", "oid": blob_oid(value), "path": "目录/a b.sh"},
        ], "普通文件、可执行位及 UTF-8 路径必须保真，路径中的合法空格不应切坏元数据")

    def test_rejects_path_traversal_before_export(self):
        for value in ("../escape", "a/../../escape", "/absolute", "a\\..\\escape", ".git/config"):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg=f"tree 必须拒绝危险路径：{value!r}"):
                    CANDIDATE.parse_tree(tree_entry(value))

    def test_rejects_symlink_submodule_and_unknown_modes(self):
        for mode, kind in (
            ("120000", "blob"), ("160000", "commit"), ("040000", "tree"),
            ("100600", "blob"), ("100644", "tree"),
        ):
            with self.subTest(mode=mode, kind=kind):
                with self.assertRaises(ValueError, msg="tree 不得接受链接、子模块或未知对象类型"):
                    CANDIDATE.parse_tree(tree_entry("fixture", mode=mode, kind=kind))

    def test_rejects_duplicate_path_even_when_oid_differs(self):
        for value in (b"same\n", b"different\n"):
            with self.subTest(value=value):
                data = tree_entry("same.txt", b"same\n") + tree_entry("same.txt", value)
                with self.assertRaises(ValueError, msg="相同路径无论 blob 是否相同都必须拒绝"):
                    CANDIDATE.parse_tree(data)

    def test_rejects_casefold_duplicate_paths_even_when_oid_differs(self):
        for first, second in (
            ("readme.md", "README.md"), ("Source/main.rs", "source/MAIN.RS"),
            ("Straße.txt", "STRASSE.TXT"),
        ):
            for left, right in ((first, second), (second, first)):
                for value in (b"same\n", b"different\n"):
                    with self.subTest(first=left, second=right, value=value):
                        data = tree_entry(left, b"same\n") + tree_entry(right, value, mode="100755")
                        with self.assertRaisesRegex(ValueError, "重复路径", msg="大小写折叠后重名必须拒绝，不能让后写文件覆盖"):
                            CANDIDATE.parse_tree(data)

    def test_rejects_windows_aliases_devices_and_invalid_characters(self):
        for value in (
            ".GIT/config", "safe/.git./config", "file.", "directory /file", "NUL.txt",
            "safe/COM¹.log", "safe/LPT9/file", "bad?name", "safe/bad|name/file",
            "目录/a\tb.sh", "safe/line\nbreak.txt",
        ):
            with self.subTest(path=value):
                with self.assertRaises(ValueError, msg=f"tree 解析阶段必须拒绝 Windows 不安全路径：{value!r}"):
                    CANDIDATE.parse_tree(tree_entry(value))

    def test_rejects_malformed_metadata_and_oid_length(self):
        for data in (
            b"no-tab\0", b"100644 blob\tfixture\0", b"\xff blob abc\tfixture\0",
            tree_entry("fixture", oid="a" * 39), tree_entry("fixture", oid="a" * 41),
            b"100644 blob " + b"a" * 40 + b"\t\xff\0",
        ):
            with self.subTest(data=data):
                with self.assertRaises(ValueError, msg="损坏的 tree 元数据和路径编码必须拒绝"):
                    CANDIDATE.parse_tree(data)

    def test_empty_tree_has_no_fabricated_entries(self):
        self.assertEqual(CANDIDATE.parse_tree(b""), [], "空 tree 不应凭空生成文件")


class ExportTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        temporary = tempfile.TemporaryDirectory(prefix="cortex-cua-export-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repository = self.root / "primary"
        self.destination = self.root / "exported"
        self.destination.mkdir()
        self.caches = (self.root / "cache-a", self.root / "cache-b")
        self.env = {"CORTEX_TEST_ONLY": "private-fixture"}
        self.value = b"frozen fixture\n\0\t"
        self.oid = blob_oid(self.value)
        self.name = "libs/cua-driver/rust/src/main.rs"
        self.tree = tree_entry(self.name, self.value)
        self.tree_command = ["git", "ls-tree", "-rz", "--full-tree", CANDIDATE.BASE, "--", *CANDIDATE.PREFIXES]

    def export_fixture(self, tree, batch, oids, *, caches=(), cache_blobs=None):
        cache_blobs = cache_blobs or {}

        def respond(args, cwd, env, data=None):
            self.assertIs(env, self.env, "所有对象读取必须沿用调用者的已清理环境")
            if args == self.tree_command:
                self.assertEqual(cwd, self.repository, "冻结 tree 只能读取主仓库")
                self.assertIsNone(data, "ls-tree 不应接收替代输入")
                return tree
            if args == ["git", "cat-file", "--batch"]:
                self.assertEqual(cwd, self.repository, "批量对象查询必须首先读取主仓库")
                self.assertEqual(data, "".join(oid + "\n" for oid in oids).encode("ascii"),
                                 "只能请求主 tree 明确列出的对象，且顺序必须一致")
                return batch
            if len(args) == 4 and args[:3] == ["git", "cat-file", "blob"]:
                self.assertIn(cwd, caches, "缺失对象只能查显式列出的本地缓存")
                self.assertIn(args[3], oids, "缓存请求必须绑定主 tree 的对象 ID")
                self.assertIsNone(data, "缓存读取不得接收替代提交或对象输入")
                key = (cwd, args[3])
                self.assertIn(key, cache_blobs, "每次缓存读取都必须有明确的内存响应")
                response = cache_blobs[key]
                if isinstance(response, Exception):
                    raise response
                return response
            self.fail(f"测试不允许未声明的命令：{args!r}")

        self.command.side_effect = respond
        return CANDIDATE.export(self.repository, self.destination, self.env, caches)

    def assert_no_files(self):
        self.assertEqual(list(self.destination.rglob("*")), [], "不合法对象不得写出任何文件")

    def test_git_hash_fixture_matches_known_empty_blob(self):
        self.assertEqual(blob_oid(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
                         "测试夹具必须使用真实 Git blob SHA-1")

    def test_exports_binary_blob_and_records_real_hashes(self):
        result = self.export_fixture(self.tree, batch_blob(self.value), [self.oid], caches=self.caches)
        self.assertEqual((self.destination / self.name).read_bytes(), self.value, "二进制 blob 必须逐字节导出")
        self.assertEqual(result, [{
            "mode": "100644", "oid": self.oid, "path": self.name,
            "sha256": hashlib.sha256(self.value).hexdigest(),
        }], "清单必须保留主 tree 身份和实际内容的 SHA-256")
        self.assertEqual(self.command.call_args_list, [
            mock.call(self.tree_command, self.repository, self.env),
            mock.call(["git", "cat-file", "--batch"], self.repository, self.env,
                      (self.oid + "\n").encode("ascii")),
        ], "主仓库已有 blob 时不应读取缓存或其他提交")

    def test_exports_empty_blob(self):
        result = self.export_fixture(tree_entry("empty", b""), batch_blob(b""), [blob_oid(b"")])
        self.assertEqual((self.destination / "empty").read_bytes(), b"", "空 blob 也必须创建文件")
        self.assertEqual(result[0]["sha256"], hashlib.sha256(b"").hexdigest(), "空文件哈希必须真实计算")

    def test_accepts_shared_blob_at_distinct_paths_and_keeps_batch_offset(self):
        tail = b"second blob\n"
        tree = self.tree + tree_entry("copy.txt", self.value) + tree_entry("tail.txt", tail)
        batch = batch_blob(self.value) * 2 + batch_blob(tail)
        result = self.export_fixture(tree, batch, [self.oid, self.oid, blob_oid(tail)])
        self.assertEqual(len(result), 3, "不同路径共享 blob 合法，不应误当作重复路径")
        for name, value in ((self.name, self.value), ("copy.txt", self.value), ("tail.txt", tail)):
            self.assertEqual((self.destination / name).read_bytes(), value, "batch 偏移必须按字节长度推进")

    def test_rejects_empty_primary_tree_without_consulting_cache(self):
        with self.assertRaisesRegex(ValueError, "tree 为空", msg="缓存不能为主仓库补造 tree"):
            self.export_fixture(b"", b"", [], caches=self.caches)
        self.assertEqual(self.command.call_count, 1, "主 tree 为空时必须立即终止")
        self.assert_no_files()

    def test_rejects_unsafe_tree_before_any_blob_lookup(self):
        for tree in (tree_entry("../escape"), tree_entry("link", mode="120000"), self.tree * 2):
            with self.subTest(tree=tree):
                self.command.reset_mock()
                with self.assertRaises(ValueError, msg="不安全 tree 必须在读取 blob 之前拒绝"):
                    self.export_fixture(tree, b"", [self.oid], caches=self.caches)
                self.assertEqual(self.command.call_count, 1, "不安全 tree 不得触发缓存或 batch 查询")
                self.assert_no_files()

    def test_rejects_windows_alias_tree_before_any_blob_lookup(self):
        trees = [tree_entry(value) for value in (
            ".GIT/config", "safe/.git./config", "file.", "directory /file", "NUL.txt", "bad?name",
        )]
        trees.append(tree_entry("Source/file.rs") + tree_entry("source/FILE.RS", b"replacement\n"))
        for tree in trees:
            with self.subTest(tree=tree):
                self.command.reset_mock()
                with self.assertRaises(ValueError, msg="Windows 别名或同名覆盖风险必须在读取 blob 前拒绝"):
                    self.export_fixture(tree, b"", [self.oid], caches=self.caches)
                self.assertEqual(self.command.call_count, 1, "危险路径不得触发 batch、缓存或其他命令")
                self.assert_no_files()

    def test_rejects_wrong_batch_oid_or_type_without_cache_fallback(self):
        for header in (
            f"{blob_oid(b'other')} blob {len(self.value)}\n",
            f"{self.oid} tree {len(self.value)}\n", f"{self.oid} blob\n",
            f"{self.oid} missing extra\n", f"{blob_oid(b'other')} missing\n",
        ):
            with self.subTest(header=header):
                self.command.reset_mock()
                with self.assertRaisesRegex(ValueError, "未知对象", msg="batch 对象身份或类型不符必须拒绝"):
                    self.export_fixture(self.tree, header.encode("ascii") + self.value + b"\n",
                                        [self.oid], caches=self.caches)
                self.assertEqual(self.command.call_count, 2, "协议损坏不能伪装成缺失 blob 使用缓存")
                self.assert_no_files()

    def test_rejects_forged_content_with_matching_header_and_length(self):
        forged = b"X" * len(self.value)
        self.assertNotEqual(blob_oid(forged), self.oid, "攻击夹具必须与主 tree 的真实哈希不同")
        with self.assertRaisesRegex(ValueError, "身份不匹配", msg="不能只信 batch 头声称的对象 ID"):
            self.export_fixture(self.tree, batch_blob(forged, oid=self.oid), [self.oid], caches=self.caches)
        self.assertEqual(self.command.call_count, 2, "已返回的坏 blob 不得转而读取缓存")
        self.assert_no_files()

    def test_rejects_raw_content_sha1_instead_of_git_object_sha1(self):
        raw_oid = hashlib.sha1(self.value).hexdigest()
        with self.assertRaisesRegex(ValueError, "身份不匹配", msg="必须校验 Git 对象头和内容的组合哈希"):
            self.export_fixture(tree_entry(self.name, oid=raw_oid), batch_blob(self.value, oid=raw_oid), [raw_oid])
        self.assert_no_files()

    def test_rejects_truncated_blob_and_missing_separator(self):
        header = f"{self.oid} blob {len(self.value)}\n".encode("ascii")
        for data in (header + self.value[:-1], header + self.value, header + self.value + b"!"):
            with self.subTest(data=data):
                with self.assertRaisesRegex(ValueError, "不完整", msg="blob 缺失字节或结尾换行必须拒绝"):
                    self.export_fixture(self.tree, data, [self.oid])
                self.assert_no_files()

    def test_rejects_invalid_lengths_and_missing_header_newline(self):
        for data in (
            b"", self.oid.encode("ascii"), f"{self.oid} blob nope\n".encode("ascii"),
            f"{self.oid} blob -1\n".encode("ascii"), f"{self.oid} blob 999\n".encode("ascii"),
        ):
            with self.subTest(data=data):
                with self.assertRaises(ValueError, msg="损坏的 batch 长度或头分隔必须拒绝"):
                    self.export_fixture(self.tree, data, [self.oid])
                self.assert_no_files()

    def test_rejects_unexpected_batch_tail(self):
        for tail in (b"\n", b"unexpected", batch_blob(b"unrequested blob")):
            with self.subTest(tail=tail):
                with self.assertRaisesRegex(ValueError, "未预期尾部", msg="batch 不得夹带主 tree 未请求的数据"):
                    self.export_fixture(self.tree, batch_blob(self.value) + tail, [self.oid])

    def test_missing_blob_without_cache_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "本地缺少固定 blob", msg="缺失 blob 不得触发拉取或生成 stub"):
            self.export_fixture(self.tree, f"{self.oid} missing\n".encode("ascii"), [self.oid])
        self.assertEqual(self.command.call_count, 2, "无缓存时不能再发起其他命令")
        self.assert_no_files()

    def test_missing_blob_uses_verified_cache_and_preserves_primary_tree(self):
        cache = self.caches[0]
        result = self.export_fixture(
            self.tree, f"{self.oid} missing\n".encode("ascii"), [self.oid], caches=self.caches,
            cache_blobs={(cache, self.oid): self.value},
        )
        self.assertEqual(result, [{
            "mode": "100644", "oid": self.oid, "path": self.name,
            "objectRepository": str(cache), "sha256": hashlib.sha256(self.value).hexdigest(),
        }], "缓存只能补字节，不得替换主 tree 的路径、模式、对象 ID 或提交")
        self.assertEqual((self.destination / self.name).read_bytes(), self.value, "正确缓存内容必须能导出")
        self.assertEqual(self.command.call_args_list[-1],
                         mock.call(["git", "cat-file", "blob", self.oid], cache, self.env),
                         "缓存查询必须使用主 tree 中的完整对象 ID")
        self.assertEqual(self.command.call_count, 3, "首个缓存命中后不应访问后续缓存")

    def test_missing_blob_tries_next_cache_only_after_local_failure(self):
        first, second = self.caches
        result = self.export_fixture(
            self.tree, f"{self.oid} missing\n".encode("ascii"), [self.oid], caches=self.caches,
            cache_blobs={(first, self.oid): RuntimeError("私有夹具无此对象"), (second, self.oid): self.value},
        )
        self.assertEqual(result[0]["objectRepository"], str(second), "必须记录实际提供 blob 的缓存")
        self.assertEqual(self.command.call_args_list[-2:], [
            mock.call(["git", "cat-file", "blob", self.oid], first, self.env),
            mock.call(["git", "cat-file", "blob", self.oid], second, self.env),
        ], "缓存顺序必须明确，且两次请求始终绑定同一个主 tree 对象")

    def test_missing_blob_in_all_caches_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "本地缺少固定 blob", msg="所有缓存缺失时必须失败，不能换提交"):
            self.export_fixture(
                self.tree, f"{self.oid} missing\n".encode("ascii"), [self.oid], caches=self.caches,
                cache_blobs={(cache, self.oid): RuntimeError("私有夹具无此对象") for cache in self.caches},
            )
        self.assertEqual(self.command.call_count, 4, "缓存用尽后不得查询远端或其他对象")
        self.assert_no_files()

    def test_rejects_cache_content_not_matching_primary_tree_oid(self):
        cache = self.caches[0]
        forged = b"different cached object\n"
        self.assertNotEqual(blob_oid(forged), self.oid, "缓存攻击夹具必须具有不同的真实 Git 哈希")
        with self.assertRaisesRegex(ValueError, "身份不匹配", msg="本地缓存也必须验证真实 Git blob 哈希"):
            self.export_fixture(
                self.tree, f"{self.oid} missing\n".encode("ascii"), [self.oid], caches=self.caches,
                cache_blobs={(cache, self.oid): forged},
            )
        self.assertEqual(self.command.call_count, 3, "缓存返回错误身份后应立即失败")
        self.assert_no_files()

    def test_missing_blob_keeps_next_batch_object_aligned(self):
        tail = b"next primary blob\n"
        cache = self.caches[0]
        result = self.export_fixture(
            self.tree + tree_entry("tail.txt", tail),
            f"{self.oid} missing\n".encode("ascii") + batch_blob(tail), [self.oid, blob_oid(tail)],
            caches=self.caches, cache_blobs={(cache, self.oid): self.value},
        )
        self.assertEqual((self.destination / "tail.txt").read_bytes(), tail, "补缓存后必须正确解析下一条 batch")
        self.assertNotIn("objectRepository", result[1], "主仓库已有 blob 不应被错误标注为缓存来源")


class PatchDeltaTests(IsolatedTests):
    def setUp(self):
        super().setUp()
        before = {
            "src/first.rs": b"first before\n",
            "src/second.rs": b"second before\n",
            "README.md": b"unchanged documentation\n",
        }
        after = dict(before)
        after.update({"src/first.rs": b"first after\n", "src/second.rs": b"second after\n"})
        self.original = {name: hashlib.sha256(value).hexdigest() for name, value in before.items()}
        self.tree = [
            {"path": name, "mode": "100644", "oid": blob_oid(value), "sha256": self.original[name]}
            for name, value in before.items()
        ]
        self.expected = {name: hashlib.sha256(value).hexdigest() for name, value in after.items()}
        self.patch = (
            b"diff --git a/src/first.rs b/src/first.rs\n"
            b"--- a/src/first.rs\n+++ b/src/first.rs\n"
            b"@@ -1 +1 @@\n-first before\n+first after\n"
            b"diff --git a/src/second.rs b/src/second.rs\n"
            b"--- a/src/second.rs\n+++ b/src/second.rs\n"
            b"@@ -1 +1 @@\n-second before\n+second after\n"
        )

    def test_accepts_exact_complete_delta_without_mutating_inputs(self):
        tree_before = [dict(item) for item in self.tree]
        expected_before = dict(self.expected)
        for patch in (self.patch, self.patch.replace(b"\n", b"\r\n")):
            with self.subTest(crlf=b"\r\n" in patch):
                self.assertIsNone(CANDIDATE.check_patch_delta(self.tree, self.expected, patch),
                                  "实际差异恰好覆盖全部补丁路径时必须通过，且应兼容 CRLF")
        self.assertEqual(self.tree, tree_before, "差异校验不能改写冻结 tree 的身份或哈希")
        self.assertEqual(self.expected, expected_before, "差异校验不能改写实际源码清单")
        self.assertEqual(self.command.call_count, 0, "差异校验必须保持纯内存，不得调用 Git")

    def test_rejects_unapplied_patch(self):
        with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="git apply 即使返回成功，源码完全未变也必须拒绝"):
            CANDIDATE.check_patch_delta(self.tree, self.original, self.patch)

    def test_rejects_partially_applied_patch(self):
        for unapplied in ("src/first.rs", "src/second.rs"):
            with self.subTest(unapplied=unapplied):
                incomplete = dict(self.expected)
                incomplete[unapplied] = self.original[unapplied]
                with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="补丁声明的每个文件都必须实际发生差异"):
                    CANDIDATE.check_patch_delta(self.tree, incomplete, self.patch)

    def test_rejects_additional_content_change(self):
        extra = dict(self.expected)
        extra["README.md"] = hashlib.sha256(b"unexpected documentation change\n").hexdigest()
        with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="完整补丁之外的任何额外内容差异都必须拒绝"):
            CANDIDATE.check_patch_delta(self.tree, extra, self.patch)

    def test_rejects_extra_added_or_deleted_file(self):
        for operation in ("新增", "删除"):
            with self.subTest(operation=operation):
                extra = dict(self.expected)
                if operation == "新增":
                    extra["undeclared.txt"] = hashlib.sha256(b"unexpected new file\n").hexdigest()
                else:
                    del extra["README.md"]
                with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="源码清单新增或丢失未声明文件都属于额外差异"):
                    CANDIDATE.check_patch_delta(self.tree, extra, self.patch)

    def test_rejects_wrong_paths_even_when_change_counts_match(self):
        wrong = dict(self.expected)
        wrong["src/second.rs"] = self.original["src/second.rs"]
        wrong["README.md"] = hashlib.sha256(b"changed wrong file\n").hexdigest()
        with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="改动数量相同也必须逐个核对补丁声明的路径"):
            CANDIDATE.check_patch_delta(self.tree, wrong, self.patch)

    def test_rejects_empty_or_absent_patch_declarations(self):
        for patch in (b"", b"no patch targets\n", b"--- a/src/first.rs\n+++ /dev/null\n"):
            for expected in (self.original, self.expected):
                with self.subTest(patch=patch, changed=expected is self.expected):
                    with self.assertRaisesRegex(ValueError, "完整冻结补丁", msg="没有 +++ b/ 目标的补丁不能把任何源码视作候选"):
                        CANDIDATE.check_patch_delta(self.tree, expected, patch)


class CleanEnvironmentTests(IsolatedTests):
    def test_removes_git_and_rust_injection_variables(self):
        dangerous = {
            "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM", "GIT_CONFIG_KEY_99", "GIT_CONFIG_VALUE_99", "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_PROFILE_DEV_BUILD_OVERRIDE_OPT_LEVEL", "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS", "RUSTC", "RUSTDOC",
            "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTDOC", "CARGO_BUILD_RUSTC_WRAPPER",
            "CUA_DRIVER_REVIEW_EXTENSION_PUBLIC_KEY_BASE64",
        }
        injected = dict.fromkeys(dangerous, "injected-fixture")
        with mock.patch.dict(os.environ, injected, clear=True):
            cleaned = CANDIDATE.clean_environment()
        for key in dangerous:
            with self.subTest(variable=key):
                self.assertNotIn(key, cleaned, f"必须移除可改变对象来源或编译输入的环境变量：{key}")

    def test_overrides_offline_and_frozen_source_settings(self):
        expected = {
            "GIT_NO_LAZY_FETCH": "1", "GIT_TERMINAL_PROMPT": "0", "GIT_ALLOW_PROTOCOL": "",
            "GIT_CONFIG_COUNT": "2", "GIT_CONFIG_KEY_0": "gc.auto", "GIT_CONFIG_VALUE_0": "0",
            "GIT_CONFIG_KEY_1": "maintenance.auto", "GIT_CONFIG_VALUE_1": "false",
            "CARGO_NET_OFFLINE": "true", "CARGO_INCREMENTAL": "0", "RUSTUP_AUTO_INSTALL": "0",
            "CUA_DRIVER_GIT_SHA": CANDIDATE.BASE,
        }
        with mock.patch.dict(os.environ, dict.fromkeys(expected, "injected-fixture"), clear=True):
            cleaned = CANDIDATE.clean_environment()
        self.assertEqual(cleaned, expected, "离线、禁提示、禁自动维护和冻结提交必须覆盖外部注入")

    def test_preserves_toolchain_discovery_without_mutating_parent_environment(self):
        original = {
            "PATH": "fixture-tools", "SystemRoot": "fixture-windows", "TEMP": "fixture-temp",
            "CARGO_HOME": "fixture-cargo", "RUSTUP_HOME": "fixture-rustup", "RUSTFLAGS": "injected",
        }
        with mock.patch.dict(os.environ, original, clear=True):
            before = dict(os.environ)
            cleaned = CANDIDATE.clean_environment()
            self.assertEqual(dict(os.environ), before, "清理必须返回副本，不能影响 Main 的构建环境")
            for key in before.keys() - {"RUSTFLAGS"}:
                self.assertEqual(cleaned[key], before[key], "工具链定位与私有临时目录配置必须保留")
            cleaned["PATH"] = "changed-copy"
            self.assertEqual(dict(os.environ), before, "修改返回值也不能污染父进程环境")


if __name__ == "__main__":
    unittest.main()
