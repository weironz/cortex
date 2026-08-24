import 'package:flutter/material.dart';

/// 把带 ANSI 转义序列的终端输出变成可渲染的 [TextSpan]。
///
/// # 为什么需要它
///
/// `shell` 工具的输出是**真终端的输出**：`cargo` / `npm` / `git` 一律带色。
/// 在此之前两侧谁都没处理它，于是那些序列原样进了界面 ——
/// 用户看到的是 `[32m通过[0m` 而不是一个绿色的「通过」。
/// 这是「富文本四件套」里剩下的最后一件。
///
/// # 只认颜色，其余一律丢掉
///
/// 终端控制序列里只有一小部分在这里有意义。光标移动（`[2A`）、
/// 清屏（`[2J`）、切换备用屏这些**都是对着一块可寻址的字符网格说的**，
/// 而这里是一个只会往下长的文本块 —— 照做既做不到，也没有正确的做法。
///
/// 丢掉比留着好：留着就是那串乱码，而丢掉之后至少文字是对的。
/// 进度条那类反复重画一行的输出因此会变成很多行 —— 那是**如实**的，
/// 它本来就写了那么多次。
///
/// # 不做的
///
/// 256 色与真彩（`38;5;n` / `38;2;r;g;b`）只取到「有颜色」这一层：
/// 它们的参数被吃掉，不会漏成正文。做全套要一张调色板，而终端调色板
/// 与应用主题是两套颜色体系 —— 在浅色主题上照搬终端的亮黄，等于不可读。
List<TextSpan> parseAnsi(
  String input, {
  required TextStyle base,
  Brightness brightness = Brightness.light,
}) {
  final spans = <TextSpan>[];
  final buf = StringBuffer();
  var style = base;

  void flush() {
    if (buf.isEmpty) return;
    spans.add(TextSpan(text: buf.toString(), style: style));
    buf.clear();
  }

  var i = 0;
  while (i < input.length) {
    final c = input.codeUnitAt(i);
    // ESC(0x1B) 之外的一切都是正文。**包括落单的 ESC** —— 一个不成形的
    // 序列丢掉的话，正文会莫名其妙少一段
    if (c != 0x1B || i + 1 >= input.length) {
      buf.writeCharCode(c);
      i++;
      continue;
    }
    final next = input[i + 1];
    if (next != '[') {
      // 非 CSI 的转义（`ESC]…` OSC 设标题、`ESC(B` 选字符集…）。
      // 这里只跳过 ESC 本身：真去解 OSC 要处理 BEL 与 ST 两种终止符，
      // 而它们在这个场景下没有任何可渲染的内容
      i++;
      continue;
    }
    // CSI：`ESC [` 参数 中间字节 最终字节
    var j = i + 2;
    while (j < input.length) {
      final u = input.codeUnitAt(j);
      // 参数字节 0x30-0x3F，中间字节 0x20-0x2F
      if (u >= 0x30 && u <= 0x3F || u >= 0x20 && u <= 0x2F) {
        j++;
        continue;
      }
      break;
    }
    if (j >= input.length) {
      // 截断的序列（输出被截断时很常见）。整段丢掉 ——
      // 把半个序列当正文吐出来是最难看的一种失败
      i = input.length;
      break;
    }
    final finalByte = input[j];
    final params = input.substring(i + 2, j);
    if (finalByte == 'm') {
      flush();
      style = _applySgr(style, params, base, brightness);
    }
    // 其余最终字节（A/B/C/D 光标、J/K 清除、H 定位…）：认出来并丢掉
    i = j + 1;
  }
  flush();
  return spans;
}

/// 把纯文本从 ANSI 输出里抠出来（不要样式，只要字）。
///
/// 给「一行摘要」那种场合用：那里没有地方放颜色，但**更不该放乱码**。
String stripAnsi(String input) {
  const plain = TextStyle();
  return parseAnsi(input, base: plain).map((s) => s.text ?? '').join();
}

/// SGR（`ESC[…m`）。只实现在这个界面里说得清的那几档。
TextStyle _applySgr(
  TextStyle current,
  String params,
  TextStyle base,
  Brightness brightness,
) {
  // 空参数等价于 `0`（重置）—— `ESC[m` 是合法写法，漏掉它会让重置失效，
  // 于是后面所有文字都带着上一段的颜色
  final codes = (params.isEmpty ? '0' : params)
      .split(';')
      .map((s) => int.tryParse(s) ?? 0)
      .toList();

  var style = current;
  for (var k = 0; k < codes.length; k++) {
    final code = codes[k];
    switch (code) {
      case 0:
        style = base;
      case 1:
        style = style.copyWith(fontWeight: FontWeight.bold);
      case 2:
        // dim：用透明度而不是换颜色 —— 后者会覆盖掉同时设的前景色
        style = style.copyWith(
          color: (style.color ?? base.color)?.withValues(alpha: 0.6),
        );
      case 3:
        style = style.copyWith(fontStyle: FontStyle.italic);
      case 4:
        style = style.copyWith(decoration: TextDecoration.underline);
      case 22:
        style = style.copyWith(fontWeight: base.fontWeight);
      case 23:
        style = style.copyWith(fontStyle: base.fontStyle);
      case 24:
        style = style.copyWith(decoration: TextDecoration.none);
      case 39:
        style = style.copyWith(color: base.color);
      // 38/48 是扩展色：`38;5;n` 或 `38;2;r;g;b`。
      // **必须把参数吃掉**，否则那个 n 会被当成下一个 SGR 码 ——
      // 比如 `38;5;1` 会被读成「重置 + 红」，颜色是错的且时机也错
      case 38:
      case 48:
        if (k + 1 < codes.length) {
          final mode = codes[k + 1];
          k += mode == 5 ? 2 : (mode == 2 ? 4 : 1);
        }
      default:
        final c = _sgrColor(code, brightness);
        if (c != null) style = style.copyWith(color: c);
    }
  }
  return style;
}

/// 标准 8 色与亮色的前景码 —— **按主题亮度取两组**。
///
/// 上一版是一组中等饱和度的值配一句「两种主题下都要能用」，实测那组值
/// 只在浅色底上成立：深色 `#1B1B1F` 上红 `#D64545` 约 3.5:1、灰 `#6B7280`
/// 约 3.2:1，编译报错的那几行红字恰好是深色下最读不清的。深色组取与
/// `CortexTokens` 反馈色同族的亮档（红 #F87171 / 绿 #4ADE80 / 黄 #FBBF24 /
/// 蓝 #60A5FA），两套颜色体系不再各说各话。
/// 背景色（40-47）**不给** —— 一段带背景块的文字在气泡里像是被选中了。
Color? _sgrColor(int code, Brightness brightness) {
  final dark = brightness == Brightness.dark;
  return switch (code) {
    30 || 90 => dark ? const Color(0xFF9CA3AF) : const Color(0xFF6B7280),
    31 || 91 => dark ? const Color(0xFFF87171) : const Color(0xFFD64545),
    32 || 92 => dark ? const Color(0xFF4ADE80) : const Color(0xFF2E9E5B),
    33 || 93 => dark ? const Color(0xFFFBBF24) : const Color(0xFFB7791F),
    34 || 94 => dark ? const Color(0xFF60A5FA) : const Color(0xFF3B7DD8),
    35 || 95 => dark ? const Color(0xFFC084FC) : const Color(0xFF9B51E0),
    36 || 96 => dark ? const Color(0xFF4FD1C5) : const Color(0xFF12A5A5),
    37 || 97 => dark ? const Color(0xFFD1D5DB) : const Color(0xFF9CA3AF),
    _ => null,
  };
}
