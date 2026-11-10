"""只读取 Cargo metadata 给出的实际产物根，不根据版本号或默认目录猜测。"""
import json
import posixpath
import re
import sys


def resolve_target_directory(metadata):
    if not isinstance(metadata, dict):
        raise ValueError("Cargo metadata 必须是对象")
    value = metadata.get("target_directory")
    if not isinstance(value, str) or not value or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError("Cargo target_directory 缺失或含控制字符")
    if value.startswith("\\") and not value.startswith("\\\\"):
        raise ValueError("Windows 根相对路径不是完整的绝对产物根")
    value = value.replace("\\", "/")
    if not (posixpath.isabs(value) or re.match(r"^[A-Za-z]:/", value)):
        raise ValueError("Cargo target_directory 必须是绝对路径")
    if re.fullmatch(r"[A-Za-z]:/+", value):
        return value[:3]
    return value.rstrip("/") or "/"


if __name__ == "__main__":
    try:
        print(resolve_target_directory(json.load(sys.stdin)))
    except (ValueError, TypeError):
        raise SystemExit("不能解析 Cargo 实际产物目录；禁止回落到旧 target 产物") from None
