"""从冻结 Git 对象重建候选，比较两份独立产物；不运行驱动或替换正式 pin。"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
import time

BASE = "85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19"
PATCH_HASH = "66b7966379606a2eb4fc4b3099ae482ca54f70b18e34010a184f98a99c82b17f"
TOOLCHAIN = "1.98.0-x86_64-pc-windows-msvc"
TARGET = "x86_64-pc-windows-msvc"
WORKSPACE = "libs/cua-driver/rust"
PREFIXES = ["AGENTS.md", "LICENSE.md", WORKSPACE, "libs/cua/Cargo.toml",
            "libs/cua/crates/cua-telemetry", "libs/images/sandbox-images.json",
            "libs/cua/crates/cua-teleport/src/ux/targets.json"]
ROOT = Path(__file__).resolve().parent.parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + ".next")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def clean_environment():
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("CARGO_PROFILE_", "GIT_CONFIG_", "CARGO_TARGET_")) or key in {
            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTFLAGS", "CUA_DRIVER_REVIEW_EXTENSION_PUBLIC_KEY_BASE64",
            "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES", "RUSTC", "RUSTDOC", "CARGO_BUILD_TARGET",
            "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTDOC", "CARGO_BUILD_RUSTC_WRAPPER",
        }:
            del env[key]
    env.update(GIT_NO_LAZY_FETCH="1", GIT_TERMINAL_PROMPT="0", GIT_ALLOW_PROTOCOL="",
               GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_0="gc.auto", GIT_CONFIG_VALUE_0="0",
               GIT_CONFIG_KEY_1="maintenance.auto", GIT_CONFIG_VALUE_1="false",
               CARGO_NET_OFFLINE="true", CARGO_INCREMENTAL="0", RUSTUP_AUTO_INSTALL="0",
               CUA_DRIVER_GIT_SHA=BASE)
    return env


def command(args, cwd, env, data=None):
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    result = subprocess.run(args, cwd=cwd, env=env, input=data, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, creationflags=flags, check=False)
    if result.returncode:
        raise RuntimeError(f"命令失败 {args[:3]} exit={result.returncode}: "
                           + result.stderr.decode("utf-8", errors="replace")[-2000:])
    return result.stdout


def safe_relative(value):
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or any(part in {".", "..", ".git"} for part in path.parts):
        raise ValueError("Git 对象路径不安全")
    if "\\" in value or ":" in value or str(path) != value:
        raise ValueError("Git 路径不是规范相对路径")
    # Win32 会折叠大小写和尾部点/空格；不能让树中看似不同的名称变成 .git 或覆盖旧文件。
    devices = {"con", "prn", "aux", "nul"} | {
        prefix + number for prefix in ("com", "lpt") for number in "123456789¹²³"
    }
    for part in path.parts:
        if (part.rstrip(" .") != part or part.casefold() == ".git"
                or part.split(".", 1)[0].casefold() in devices
                or any(ord(char) < 32 or char in '<>"|?*' for char in part)):
            raise ValueError("Git 路径含 Windows 别名、保留名称或非法字符")
    return path


def parse_tree(data):
    result = []
    for entry in data.split(b"\0"):
        if not entry:
            continue
        metadata, filename = entry.split(b"\t", 1)
        mode, kind, oid = metadata.decode("ascii").split()
        name = filename.decode("utf-8")
        safe_relative(name)
        if mode not in {"100644", "100755"} or kind != "blob" or len(oid) != 40:
            raise ValueError("不导出 symlink、submodule 或未知 Git 对象")
        if any(item["path"].casefold() == name.casefold() for item in result):
            raise ValueError("Git tree 出现重复路径")
        result.append({"mode": mode, "oid": oid, "path": name})
    return result


def export(repository, destination, env, object_repositories=()):
    tree = parse_tree(command(["git", "ls-tree", "-rz", "--full-tree", BASE, "--", *PREFIXES], repository, env))
    if not tree:
        raise ValueError("冻结源码 tree 为空")
    data = command(["git", "cat-file", "--batch"], repository, env,
                   "".join(item["oid"] + "\n" for item in tree).encode("ascii"))
    offset = 0
    for item in tree:
        end = data.index(b"\n", offset)
        header = data[offset:end].decode("ascii").split()
        if header == [item["oid"], "missing"]:
            offset = end + 1
            value = None
            for fallback in object_repositories:
                try:
                    value = command(["git", "cat-file", "blob", item["oid"]], fallback, env)
                    item["objectRepository"] = str(fallback)
                    break
                except RuntimeError:
                    continue
            if value is None:
                raise ValueError("本地缺少固定 blob：" + item["path"] + "；禁止 lazy fetch、stub 或换提交")
            length = len(value)
        else:
            if len(header) != 3 or header[:2] != [item["oid"], "blob"]:
                raise ValueError("Git batch 返回未知对象")
            length = int(header[2])
            value = data[end + 1:end + 1 + length]
            offset = end + length + 2
            if len(value) != length or data[offset - 1:offset] != b"\n":
                raise ValueError("Git batch blob 不完整")
        actual = hashlib.sha1(b"blob " + str(length).encode("ascii") + b"\0" + value).hexdigest()
        if actual != item["oid"]:
            raise ValueError("Git blob 身份不匹配")
        file = destination.joinpath(*safe_relative(item["path"]).parts)
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(value)
        item["sha256"] = sha(value)
    if offset != len(data):
        raise ValueError("Git batch 有未预期尾部")
    return tree


def inventory(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("构建源码不接受链接")
        if path.is_file():
            result[path.relative_to(root).as_posix()] = sha(path.read_bytes())
    return result


def check_patch_delta(tree, expected, patch):
    original = {item["path"]: item["sha256"] for item in tree}
    changed = {name for name in original.keys() | expected.keys() if original.get(name) != expected.get(name)}
    declared = set(re.findall(r"^\+\+\+ b/(.+)$", patch.decode("utf-8"), flags=re.MULTILINE))
    declared = {name.rstrip("\r") for name in declared}
    if not declared or changed != declared:
        raise ValueError("实际源码差异不是完整冻结补丁；不能把忽略 patch 的导出当作候选")


def build(number, output, prepared, expected, environment, jobs):
    # 固定构建路径以排除生成文件/工具链路径的影响，完成后整体归档再从零重建。
    # 归档保留全部中间件，不共享旧 target，也不把删除目录当作干净构建证明。
    folder = output / "active-build"
    archive = output / f"build-{number}"
    folder.mkdir()
    source = folder / "source"
    shutil.copytree(prepared, source)
    if inventory(source) != expected:
        raise ValueError("独立构建输入复制不一致")
    env = dict(environment)
    env.update(CARGO_TARGET_DIR=str(folder / "target"), CARGO_BUILD_JOBS=str(jobs),
               GIT_CEILING_DIRECTORIES=str(output),
               CARGO_ENCODED_RUSTFLAGS="\x1f".join([
                   "-C", "target-feature=+crt-static", "-C", "link-arg=/Brepro",
                   "--remap-path-prefix=" + folder.as_posix() + "=/cua/build",
               ]))
    args = ["rustup", "run", TOOLCHAIN, "cargo", "build", "--frozen", "--release",
            "--target", TARGET, "-p", "cua-driver", "--bin", "cua-driver", "--jobs", str(jobs)]
    started = time.time()
    progress = output / "progress.json"
    with (folder / "build.log").open("wb") as log:
        process = subprocess.Popen(args, cwd=source / WORKSPACE, env=env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        while True:
            state = {"state": "building", "build": number, "pid": process.pid,
                     "elapsedSeconds": round(time.time() - started), "log": str(folder / "build.log")}
            write_json(progress, state)
            try:
                code = process.wait(timeout=10)
                break
            except subprocess.TimeoutExpired:
                print(json.dumps(state), flush=True)
    same = inventory(source) == expected
    binary = folder / "target" / TARGET / "release/cua-driver.exe"
    result = {"build": number, "exitCode": code, "sourceUnchanged": same,
              "elapsedSeconds": round(time.time() - started, 3), "command": args,
              "binary": str(binary), "sha256": sha(binary.read_bytes()) if code == 0 and binary.is_file() else None,
              "driverExecuted": False, "logSha256": sha((folder / "build.log").read_bytes())}
    write_json(folder / "result.json", result)
    if code or not same or not result["sha256"]:
        raise RuntimeError("独立构建失败或修改了源码，保留日志及所有现场")
    if (folder.resolve().parent != output.resolve() or archive.absolute().parent != output.resolve()
            or archive.exists() or folder.is_symlink()):
        raise ValueError("归档目标不在本次输出根，拒绝移动")
    folder.rename(archive)
    result["binary"] = str(archive / "target" / TARGET / "release/cua-driver.exe")
    write_json(archive / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True, help="含冻结提交及 blobs 的本地上游仓库")
    parser.add_argument("--object-repository", type=Path, action="append", default=[],
                        help="仅为缺失 blob 提供本地缓存；仍须匹配主仓库冻结 tree 中的对象 ID")
    parser.add_argument("--output", type=Path, help="必须不存在的输出目录；默认系统临时盘新目录")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--jobs", type=int, choices=range(1, 5), default=2)
    args = parser.parse_args()
    if os.name != "nt" and not args.prepare_only:
        parser.error("完整候选构建要求 Windows MSVC")
    output = args.output.absolute() if args.output else Path(tempfile.gettempdir()) / ("cortex-cua-repro-" + str(time.time_ns()))
    output.mkdir(parents=True, exist_ok=False)
    env = clean_environment()
    # --output 可以位于 Cortex checkout 内；禁止 git apply 发现外层仓库而忽略或错置补丁。
    env["GIT_CEILING_DIRECTORIES"] = str(output)
    patch = ROOT / "third_party/cua/patches/0001-windows-uia-owned-references.patch"
    if sha(patch.read_bytes()) != PATCH_HASH:
        raise ValueError("补丁不是冻结版本，不能沿用当前构建契约")
    prepared = output / "prepared-source"
    prepared.mkdir()
    tree = export(args.repository.resolve(strict=True), prepared, env,
                  [path.resolve(strict=True) for path in args.object_repository])
    command(["git", "apply", "--check", str(patch)], prepared, env)
    command(["git", "apply", str(patch)], prepared, env)
    expected = inventory(prepared)
    check_patch_delta(tree, expected, patch.read_bytes())
    shutil.copyfile(Path(__file__), output / "generator.py")
    stamp = command(["git", "show", "-s", "--format=%ct", BASE], args.repository, env).decode("ascii").strip()
    env["SOURCE_DATE_EPOCH"] = str(int(stamp))
    result = {"schema": 1, "baseCommit": BASE, "patchSha256": PATCH_HASH, "toolchain": TOOLCHAIN,
              "generatorSha256": sha(Path(__file__).read_bytes()),
              "recordedAt": datetime.now(timezone.utc).isoformat(), "sourceDateEpoch": stamp,
              "comparisonScope": "same_host_toolchain_and_canonical_build_path_two_clean_targets",
              "upstreamFiles": tree, "patchedInputs": expected, "builds": [],
              "driverExecuted": False, "productionPinChanged": False, "byteReproducibilityProven": False}
    write_json(output / "inputs.json", result)
    print(json.dumps({"output": str(output), "upstreamFiles": len(tree), "patchedFiles": len(expected)}), flush=True)
    if args.prepare_only:
        write_json(output / "progress.json", {"state": "prepared_only", "byteReproducibilityProven": False})
        return
    result["rustc"] = command(["rustup", "run", TOOLCHAIN, "rustc", "-Vv"], prepared, env).decode("utf-8")
    try:
        for number in (1, 2):
            result["builds"].append(build(number, output, prepared, expected, env, args.jobs))
            write_json(output / "result.json", result)
        result["byteReproducibilityProven"] = result["builds"][0]["sha256"] == result["builds"][1]["sha256"]
        write_json(output / "result.json", result)
        write_json(output / "progress.json", {"state": "finished", "byteReproducibilityProven": result["byteReproducibilityProven"]})
        if not result["byteReproducibilityProven"]:
            raise RuntimeError("两次构建字节不同，不能宣称可复现；保留两份产物待定位")
    except BaseException as error:
        write_json(output / "progress.json", {"state": "failed", "error": str(error), "byteReproducibilityProven": False})
        raise
    print(json.dumps({"byteReproducibilityProven": True, "sha256": result["builds"][0]["sha256"]}), flush=True)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    main()
