/// `SKILL.md` —— 跨厂商通用的那份技能格式。
///
/// # 为什么值得兼容
///
/// Claude Code / Codex / Grok Build 三家已经收敛到同一份 Agent Skills
/// 格式：一个 YAML frontmatter（`name` / `description`）加一段 markdown
/// 正文。**同一份 skill 三家都能装** —— 于是它成了事实标准，而兼容它
/// 的成本只是这一个文件。
///
/// 不兼容的代价不是「少一个功能」，是**用户已经写好的那些技能进不来**：
/// 他要为 Cortex 手抄一遍，而抄完之后两份还会各自漂移。
///
/// # 解析得宽，写出得规矩
///
/// 读的时候尽量认（frontmatter 可缺、字段顺序随意、引号可有可无），
/// 写的时候只输出最规范的一种。理由与仓库里 MCP 配置那条一样：
/// 我们读的是别人写的文件，写的是要被别人读的文件。
library;

/// 一份从 `SKILL.md` 解出来的技能。
class SkillMd {
  const SkillMd({
    required this.name,
    required this.description,
    required this.instructions,
  });

  final String name;
  final String description;
  final String instructions;
}

/// 解析一份 `SKILL.md`。
///
/// 缺 frontmatter 时**不报错**：拿第一行标题当名字、整篇当正文 ——
/// 一份手写的 markdown 做法照样能用起来。名字实在找不到就返回 `null`，
/// 由调用方说一句人话（无名的技能取不回来，见 skills 表那条唯一约束）。
SkillMd? parseSkillMd(String source, {String? fallbackName}) {
  final text = source.replaceAll('\r\n', '\n');
  var name = '';
  var description = '';
  var body = text;

  // ── frontmatter ──
  //
  // 只认**文件开头**那一段（前面允许 BOM 与空行）。不限制位置的话，
  // 正文里一段 `---` 分隔线会被当成 frontmatter 的开头
  final trimmedLeading = text.replaceFirst(RegExp(r'^﻿?\s*'), '');
  if (trimmedLeading.startsWith('---\n')) {
    final end = trimmedLeading.indexOf('\n---', 4);
    if (end > 0) {
      final front = trimmedLeading.substring(4, end);
      body = trimmedLeading.substring(end + 4).replaceFirst(RegExp(r'^\n'), '');
      for (final line in front.split('\n')) {
        final at = line.indexOf(':');
        if (at <= 0) continue;
        final key = line.substring(0, at).trim().toLowerCase();
        // 值可能带引号，也可能带行尾注释 —— 引号去掉，注释不动
        // （`description: 处理 CSV # 表格` 里那个 # 是内容的一部分）
        final value = _unquote(line.substring(at + 1).trim());
        if (key == 'name') name = value;
        if (key == 'description') description = value;
      }
    }
  }

  // ── 没有 frontmatter 时的兜底 ──
  if (name.isEmpty) {
    final heading = RegExp(r'^#\s+(.+)$', multiLine: true).firstMatch(body);
    name = heading?.group(1)?.trim() ?? fallbackName?.trim() ?? '';
  }
  if (name.isEmpty) return null;

  return SkillMd(
    name: name,
    description: description,
    instructions: body.trim(),
  );
}

/// 写成一份 `SKILL.md`。
String toSkillMd({
  required String name,
  required String description,
  required String instructions,
}) {
  final buf = StringBuffer('---\n');
  buf.writeln('name: ${_quoteIfNeeded(name)}');
  if (description.trim().isNotEmpty) {
    buf.writeln('description: ${_quoteIfNeeded(description)}');
  }
  buf
    ..writeln('---')
    ..writeln();
  buf.writeln(instructions.trim());
  return buf.toString();
}

String _unquote(String v) {
  if (v.length >= 2) {
    final first = v[0];
    if ((first == '"' || first == "'") && v.endsWith(first)) {
      return v.substring(1, v.length - 1);
    }
  }
  return v;
}

/// 会破坏 YAML 的字符才加引号。
///
/// 冒号后跟空格是 YAML 的键值分隔，不引起来的话
/// `name: 处理 CSV: 分列` 会被别的解析器读成一个嵌套映射 ——
/// 我们自己读得回来，但这份文件是**写给别人读的**。
String _quoteIfNeeded(String v) {
  final needs =
      v.contains(': ') ||
      v.contains('#') ||
      v.startsWith('"') ||
      v.startsWith("'") ||
      v.trim() != v;
  if (!needs) return v;
  return '"${v.replaceAll(r'\', r'\\').replaceAll('"', r'\"')}"';
}
