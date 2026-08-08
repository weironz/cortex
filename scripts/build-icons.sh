#!/usr/bin/env bash
#
# 从 assets/brand/*.svg 生成两端要用的全部图标。
#
# 为什么要有这个脚本，而不是把 PNG 直接提交完事：
# PNG 也确实要提交（CI 不装 ImageMagick，Flutter 构建时它们必须已经在），
# 但**没有生成方式的二进制资源改不动** —— 想调一下弧线粗细，就得重新
# 找一遍当初用什么工具、什么参数导的。这里把那份参数写下来。
#
# 依赖：ImageMagick 7（`magick`），且带 librsvg delegate。
#   magick -list format | grep SVG   # 要看到 "RSVG"
# 只有 MSVG（ImageMagick 自带的简易渲染器）的话，圆头线帽会画错，
# 弧线两端变成方的 —— 能出图，但不是这个设计。
#
# 用法：./scripts/build-icons.sh   （在仓库根目录）

set -euo pipefail

cd "$(dirname "$0")/.."

BRAND=assets/brand
MARK=$BRAND/cortex-mark.svg
SMALL=$BRAND/cortex-mark-small.svg
TILE=$BRAND/cortex-tile.svg
MASKABLE=$BRAND/cortex-tile-maskable.svg

command -v magick >/dev/null || { echo "缺 ImageMagick 7（magick）" >&2; exit 1; }
magick -list format | grep -q 'RSVG' || {
    echo "ImageMagick 没有 librsvg delegate —— 圆头线帽会被画成方的" >&2
    exit 1
}

# 小尺寸切到简化版的分界线。
#
# ≤24 用 cortex-mark-small.svg（外环 + 实心点），≥32 用母版（两道弧）。
# 这条线是**看出来的**，不是拍的：母版在 16px 下内环糊成一团，
# 24px 勉强可读，32px 完全清楚。生成完可以自己核一遍：
#   magick app/windows/runner/resources/app_icon.ico[0] -filter point -resize 900% /tmp/x.png
SMALL_MAX=24

render() {  # render <svg> <size> <out>
    magick -background none "$1" -resize "${2}x${2}" "$3"
}

# 按尺寸自动选母版还是简化版
mark_for() {  # mark_for <size>
    [ "$1" -le "$SMALL_MAX" ] && echo "$SMALL" || echo "$MARK"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# ── Windows 应用图标 + 安装程序图标 ────────────────────────
#
# 同一个文件两处用：Flutter runner 的窗口/任务栏图标，以及
# scripts/windows/cortex.iss 的 SetupIconFile（由 release-desktop-windows.sh
# 用 /DIconFile= 传进去）。所以这一个文件改了，安装包的图标跟着改。
ICO_SIZES="16 24 32 48 64 128 256"
ico_layers=()
for s in $ICO_SIZES; do
    render "$(mark_for "$s")" "$s" "$tmp/ico-$s.png"
    ico_layers+=("$tmp/ico-$s.png")
done
magick "${ico_layers[@]}" app/windows/runner/resources/app_icon.ico
echo "✓ app/windows/runner/resources/app_icon.ico（$ICO_SIZES）"

# ── Web 标签页 ────────────────────────────────────────────
#
# 同时给 .ico 和 .png：
#   favicon.ico —— 浏览器在 16px 那一档会**挑 16px 那张**，
#                  于是标签页上拿到的是简化版，不是把母版硬缩下去的糊图。
#                  这正是 .ico 这个格式唯一还值得存在的理由。
#   favicon.png —— 32px，给只认 PNG 的地方（以及 index.html 里那条
#                  历史悠久的 <link>，留着不删是因为它没坏）
FAVICON_SIZES="16 24 32 48"
fav_layers=()
for s in $FAVICON_SIZES; do
    render "$(mark_for "$s")" "$s" "$tmp/fav-$s.png"
    fav_layers+=("$tmp/fav-$s.png")
done
magick "${fav_layers[@]}" app/web/favicon.ico
render "$MARK" 32 app/web/favicon.png
echo "✓ app/web/favicon.ico（$FAVICON_SIZES）+ favicon.png（32）"

# ── PWA / 安装图标 ────────────────────────────────────────
for s in 192 512; do
    render "$TILE" "$s" "app/web/icons/Icon-$s.png"
    render "$MASKABLE" "$s" "app/web/icons/Icon-maskable-$s.png"
done
echo "✓ app/web/icons/Icon-{192,512}.png + Icon-maskable-{192,512}.png"

echo
echo "都生成好了。想肉眼核一遍小尺寸："
echo "  magick app/web/favicon.ico[0] -filter point -resize 900% /tmp/favicon-16.png"
