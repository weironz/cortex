import 'dart:convert';

import '../models/attachment.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/tool_call.dart';

/// 导出成什么。
enum ExportFormat {
  /// 给人看的。贴进笔记、发给别人、存档。
  markdown,

  /// 给程序看的。**保留 id 与工具调用的结构**，Markdown 那份会把它们压平。
  json;

  String get extension => this == ExportFormat.markdown ? 'md' : 'json';
  String get label => this == ExportFormat.markdown ? 'Markdown' : 'JSON';
}

/// 把一段会话渲染成一个文件。
///
/// # 为什么不在界面里现拼字符串
///
/// 导出是**唯一**一个会被拿去存档、发给别人、甚至当证据的产物 —— 它必须
/// 逐条可测。把它塞在按钮的 onPressed 里，那就只能靠人眼看一遍导出来的
/// 文件对不对，而「少了工具调用那几行」这种缺失在肉眼下几乎看不出来。
class SessionExport {
  const SessionExport({
    required this.session,
    required this.episodes,
    required this.exportedAt,
  });

  final ChatSession session;

  /// **整段**历史，从老到新。不是当前屏幕上那一页 —— 见
  /// `SessionExporter` 的注释。
  final List<Episode> episodes;

  /// 导出时刻。写进文件头 —— 一份没有时间的存档，三个月后没人说得清
  /// 它是哪一次导的。
  final DateTime exportedAt;

  /// 建议的文件名（不含扩展名）。
  ///
  /// 标题里的路径分隔符与控制字符要换掉：一个叫 `报告 2026/08` 的会话
  /// 在 Windows 上存不出来，而失败信息是操作系统给的、看不出跟标题有关。
  String baseName() {
    final raw = session.title.trim();
    final cleaned = raw
        .replaceAll(RegExp(r'[\\/:*?"<>|\x00-\x1f]'), '_')
        .trim();
    // 全是非法字符（或者空标题）时回落到 id，而不是导出一个叫 `___` 的文件
    if (cleaned.isEmpty || cleaned.replaceAll('_', '').trim().isEmpty) {
      return 'cortex-${session.id}';
    }
    return cleaned.length > 60 ? cleaned.substring(0, 60).trim() : cleaned;
  }

  String fileName(ExportFormat format) => '${baseName()}.${format.extension}';

  String render(ExportFormat format) => switch (format) {
    ExportFormat.markdown => _markdown(),
    ExportFormat.json => _json(),
  };

  // ------------------------------------------------------------- Markdown

  String _markdown() {
    final b = StringBuffer()
      ..writeln('# ${session.title}')
      ..writeln()
      ..writeln('> 导出自 Cortex · ${_stamp(exportedAt)}');
    if (session.workspace != null) {
      b.writeln('> 工作区：`${session.workspace}`');
    }
    b
      ..writeln('> 会话 id：`${session.id}`')
      ..writeln();

    for (final ep in episodes) {
      b
        ..writeln('---')
        ..writeln();
      final who = switch (ep.role) {
        'user' => '你',
        'assistant' => 'Cortex',
        final other => other,
      };
      final when = ep.occurredAt == null ? '' : ' · ${_stamp(ep.occurredAt!)}';
      b
        ..writeln('### $who$when')
        ..writeln();

      if (ep.attachments.isNotEmpty) {
        for (final a in ep.attachments) {
          b.writeln('- 📎 ${_attachmentLabel(a)}');
        }
        b.writeln();
      }

      if (ep.text.trim().isNotEmpty) {
        b
          ..writeln(ep.text.trim())
          ..writeln();
      }

      // 工具调用**要写进去**。少了它们，一份「agent 帮我改了三个文件」的
      // 存档里只剩下「好的，改完了」—— 而改了什么正是这份存档的价值
      if (ep.toolCalls.isNotEmpty) {
        b.writeln('<details><summary>工具调用（${ep.toolCalls.length}）</summary>');
        b.writeln();
        for (final t in ep.toolCalls) {
          b.writeln('- `${t.name}`${_toolDetail(t)}');
        }
        b
          ..writeln()
          ..writeln('</details>')
          ..writeln();
      }
    }
    return b.toString();
  }

  static String _attachmentLabel(Attachment a) {
    final name = (a.filename ?? '').trim();
    final shown = name.isEmpty ? a.hash : name;
    // 哈希也写上：附件的字节没有跟着导出来，而哈希是把它找回来的唯一线索
    return name.isEmpty ? '`$shown`' : '$shown（`${a.hash}`）';
  }

  static String _toolDetail(ToolCall t) {
    final bits = <String>[];
    if ((t.path ?? '').isNotEmpty) bits.add('path=${t.path}');
    final result = (t.result ?? '').trim();
    if (result.isNotEmpty) bits.add(result);
    // 失败要写出来。只写「调用了 write_file」的话，一份存档会让人以为
    // 那个文件写成了 —— 而它可能被沙箱拒了
    if (t.failed) bits.add('失败');
    return bits.isEmpty ? '' : ' —— ${bits.join(' · ')}';
  }

  // ----------------------------------------------------------------- JSON

  String _json() {
    final map = {
      'exported_at': exportedAt.toUtc().toIso8601String(),
      // 版本号从 1 开始。没有它的话，将来改了形状的那一天，读旧文件的人
      // 只能靠猜字段在不在
      'format_version': 1,
      'session': {
        'id': session.id,
        'title': session.title,
        'archived': session.archived,
        if (session.workspace != null) 'workspace': session.workspace,
        if (session.projectId != null) 'project_id': session.projectId,
      },
      'episodes': [
        for (final ep in episodes)
          {
            'id': ep.id,
            'role': ep.role,
            'text': ep.text,
            if (ep.occurredAt != null)
              'occurred_at': ep.occurredAt!.toUtc().toIso8601String(),
            if (ep.attachments.isNotEmpty)
              'attachments': [
                for (final a in ep.attachments)
                  {
                    'hash': a.hash,
                    if (a.filename != null) 'filename': a.filename,
                    if (a.mime != null) 'mime': a.mime,
                    if (a.sizeBytes != null) 'size_bytes': a.sizeBytes,
                  },
              ],
            if (ep.toolCalls.isNotEmpty)
              'tool_calls': [
                for (final t in ep.toolCalls)
                  {
                    'name': t.name,
                    if (t.result != null) 'result': t.result,
                    if (t.path != null) 'path': t.path,
                    if (t.failed) 'failed': true,
                  },
              ],
          },
      ],
    };
    // 缩进两格：这份文件是给人打开看的，压成一行之后 diff 与 grep 都没法用
    return const JsonEncoder.withIndent('  ').convert(map);
  }

  static String _stamp(DateTime t) {
    final l = t.toLocal();
    String two(int n) => n.toString().padLeft(2, '0');
    return '${l.year}-${two(l.month)}-${two(l.day)} '
        '${two(l.hour)}:${two(l.minute)}';
  }
}
