"""把 goose 的供应商层**取件**进 crates/cortex-providers。

⚠️ 这个脚本是**一次性**的，跑完就该只留着当记录。取件之后那份代码是我们
自己的：改它不必看上游，也不打算再从上游同步（见 NOTICE）。留着脚本是为了
让「当初取了哪些、砍了哪些」这件事有据可查 —— 而不是为了再跑一遍。

砍掉的那些不是「暂时不要」，是**我们一行都不会执行**：只发 4 个引擎
（openai / anthropic / google / ollama），databricks / snowflake 那几套
连定义都没有。
"""

import io
import os
import re
import shutil
import sys

REG = os.path.join(
    os.environ["USERPROFILE"],
    ".cargo",
    "registry",
    "src",
    "index.crates.io-1949cf8c6b5b557f",
)
TYPES = os.path.join(REG, "goose-provider-types-0.1.0-alpha.1", "src")
PROVS = os.path.join(REG, "goose-providers-0.1.0-alpha.1", "src")
OUT = os.path.join("crates", "cortex-providers", "src")

# ── 从类型层取：这些是 Message / 格式转换 / 模型目录 ──────────────
TYPES_FILES = [
    "base.rs",
    "canonical.rs",
    "conversation.rs",
    "errors.rs",
    "formats.rs",
    "goose_mode.rs",
    "images.rs",
    "json.rs",
    "mcp_utils.rs",
    "model.rs",
    "permission.rs",
    "request_log.rs",
    "retry.rs",
    "thinking.rs",
    "utils.rs",
]
TYPES_DIRS = {
    "conversation": None,  # 全要
    "canonical": None,  # 全要（含 3MB 目录 JSON）
    # formats 只要三种引擎用得到的
    # ⚠️ `openai_responses` 一度被判成「我们不走 responses API，砍掉」——
    #    错的：`openai.rs` 与 `openai_compatible.rs` 都在用它（o 系列与
    #    gpt-5 走的就是那条）。砍掉之后 ChatGPT 那家直接编不过。
    #    判据仍然是「有没有真实调用链能到达」，只是我第一次没查全。
    "formats": [
        "anthropic.rs",
        "google.rs",
        "openai.rs",
        "openai_responses.rs",
        "ollama.rs",
    ],
}

# ── 从 providers 层取：真正发 HTTP 的那些 ────────────────────────
PROVS_FILES = [
    "anthropic.rs",
    "api_client.rs",
    "declarative.rs",
    "google.rs",
    "http_status.rs",
    "ollama.rs",
    "openai.rs",
    "openai_compatible.rs",
]
PROVS_DIRS = {"declarative": None}

HEADER = """// 取件自 goose（https://github.com/block/goose），Apache-2.0，
// Copyright Block, Inc. 见仓库根目录的 NOTICE。
//
// **这份代码此后由 Cortex 自行维护，不再与上游同步。** 改它按本仓库的
// 判据来，不必去看上游怎么写 —— 那边的分支已经不是我们这份的未来。
"""


def write_text(path, text):
    """写文本，一律 LF —— 仓库里的 .rs 是 LF，Windows 上不这么写会全文件变更。"""
    io.open(path, "w", encoding="utf-8", newline="\n").write(text)


def copy_one(src, dst):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if src.endswith(".rs"):
        body = io.open(src, encoding="utf-8").read()
        io.open(dst, "w", encoding="utf-8", newline="\n").write(HEADER + body)
    else:
        shutil.copyfile(src, dst)


def main():
    # lib.rs 是**我们自己写的**（模块声明 + 取件说明），不是取件来的。
    # 重跑时要把它留下来 —— 第一次就是没留，白重写了一遍。
    keep_lib = None
    lib = os.path.join(OUT, "lib.rs")
    if os.path.exists(lib):
        keep_lib = io.open(lib, encoding="utf-8").read()
    if os.path.exists(OUT):
        shutil.rmtree(OUT)
    os.makedirs(OUT)
    if keep_lib is not None:
        write_text(lib, keep_lib)

    taken, skipped = [], []

    for f in TYPES_FILES:
        copy_one(os.path.join(TYPES, f), os.path.join(OUT, f))
        taken.append(f)
    for d, keep in TYPES_DIRS.items():
        for name in sorted(os.listdir(os.path.join(TYPES, d))):
            src = os.path.join(TYPES, d, name)
            if os.path.isdir(src):  # canonical/data
                shutil.copytree(src, os.path.join(OUT, d, name))
                taken.append(f"{d}/{name}/")
                continue
            if keep is not None and name not in keep:
                skipped.append(f"{d}/{name}")
                continue
            copy_one(src, os.path.join(OUT, d, name))
            taken.append(f"{d}/{name}")

    for f in PROVS_FILES:
        copy_one(os.path.join(PROVS, f), os.path.join(OUT, f))
        taken.append(f)
    for d, keep in PROVS_DIRS.items():
        for root, _, files in os.walk(os.path.join(PROVS, d)):
            for name in files:
                src = os.path.join(root, name)
                rel = os.path.relpath(src, PROVS)
                copy_one(src, os.path.join(OUT, rel))
                taken.append(rel.replace("\\", "/"))

    # providers 层原本把类型层当外部 crate 引；合成一个 crate 之后是 `crate::`
    for root, _, files in os.walk(OUT):
        for name in files:
            if not name.endswith(".rs"):
                continue
            p = os.path.join(root, name)
            s = io.open(p, encoding="utf-8").read()
            n = s
            n = n.replace("goose_provider_types::", "crate::")
            n = n.replace("goose_providers::", "crate::")
            n = re.sub(r"\buse goose_provider_types\b", "use crate", n)
            if n != s:
                io.open(p, "w", encoding="utf-8", newline="\n").write(n)

    print(f"取了 {len(taken)} 个文件")
    print(f"砍了 {len(skipped)}：{', '.join(skipped)}")


if __name__ == "__main__":
    sys.exit(main())
