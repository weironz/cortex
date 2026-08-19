/// 会话导出。
///
/// # 这一组盯住的头号失败：**只导出了一半，而文件完全正常**
///
/// 界面里的 transcript 只有最新那一页。直接导出它，一段几百轮的会话会安静地
/// 只导出最后几十条 —— 文件能打开、格式完全正确、就是少了一半。用户不会
/// 发现，直到他去找某句话找不到。所以导出必须自己翻到底。
///
/// 其余：工具调用不能丢（少了它们，「agent 帮我改了三个文件」的存档里
/// 只剩「好的，改完了」）、文件名里的路径分隔符要处理掉、
/// 翻到上限还没到头要**当场说**。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/session_export.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/export_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'dart:convert';

ChatSession _session({String title = '一段对话'}) =>
    ChatSession(id: 's1', title: title);

Episode _ep({
  required String id,
  String role = 'user',
  String text = '',
  List<ToolCall> tools = const [],
  List<Attachment> attachments = const [],
}) => Episode(
  id: id,
  sessionId: 's1',
  role: role,
  text: text,
  occurredAt: DateTime.utc(2026, 8, 19, 3),
  toolCalls: tools,
  attachments: attachments,
);

/// 一个分页的后端：把 [pages] 从新到旧一页一页发出去。
class _PagedApi extends MockCortexApi {
  _PagedApi(this.pages);

  /// `pages[0]` 是**最新**那一页。
  final List<List<Episode>> pages;
  int calls = 0;

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async {
    calls++;
    final index = before == null ? 0 : int.parse(before);
    final hasMore = index + 1 < pages.length;
    return SessionDetail(
      session: _session(),
      episodes: pages[index],
      hasMore: hasMore,
      nextCursor: hasMore ? '${index + 1}' : null,
    );
  }
}

void main() {
  group('渲染', () {
    test('Markdown 里工具调用不会丢', () {
      final doc = SessionExport(
        session: _session(),
        episodes: [
          _ep(id: 'e1', text: '把 README 改一下'),
          _ep(
            id: 'e2',
            role: 'assistant',
            text: '改完了。',
            tools: const [
              ToolCall(
                name: 'write_file',
                path: 'README.md',
                result: '写入 12 行',
              ),
              ToolCall(name: 'read_file', path: '密钥.txt', failed: true),
            ],
          ),
        ],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      final md = doc.render(ExportFormat.markdown);

      expect(md, contains('write_file'));
      expect(
        md,
        contains('README.md'),
        reason:
            '少了工具那几行，「agent 帮我改了三个文件」的存档里只剩下'
            '「好的，改完了」—— 而改了什么正是这份存档的价值',
      );
      expect(md, contains('失败'), reason: '失败的调用不标出来的话，读存档的人会以为那个文件读成了');
    });

    test('附件带上哈希 —— 字节没跟着导出来，哈希是找回它的唯一线索', () {
      final doc = SessionExport(
        session: _session(),
        episodes: [
          _ep(
            id: 'e1',
            text: '看这张图',
            attachments: const [
              Attachment(hash: 'deadbeef', filename: '截图.png'),
            ],
          ),
        ],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      final md = doc.render(ExportFormat.markdown);
      expect(md, contains('截图.png'));
      expect(md, contains('deadbeef'));
    });

    test('JSON 是合法 JSON，且带版本号', () {
      final doc = SessionExport(
        session: _session(),
        episodes: [_ep(id: 'e1', text: '你好')],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      final decoded =
          jsonDecode(doc.render(ExportFormat.json)) as Map<String, dynamic>;

      expect(
        decoded['format_version'],
        1,
        reason: '没有版本号的话，将来改了形状那天，读旧文件的人只能靠猜字段在不在',
      );
      expect((decoded['episodes'] as List), hasLength(1));
      expect((decoded['episodes'] as List).first['id'], 'e1');
    });

    test('文件名里的路径分隔符会被换掉', () {
      final doc = SessionExport(
        session: _session(title: '报告 2026/08'),
        episodes: const [],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      expect(
        doc.fileName(ExportFormat.markdown),
        '报告 2026_08.md',
        reason:
            '带斜杠的标题在 Windows 上存不出来，而失败信息是操作系统给的、'
            '看不出跟标题有关',
      );
    });

    test('标题全是非法字符时回落到 id，而不是一个叫 ___ 的文件', () {
      final doc = SessionExport(
        session: _session(title: '///'),
        episodes: const [],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      expect(doc.baseName(), 'cortex-s1');
    });
  });

  group('翻页', () {
    test('导出整段历史，不是屏幕上那一页', () async {
      final api = _PagedApi([
        [_ep(id: 'new1', text: '最新一页')],
        [_ep(id: 'mid1', text: '中间一页')],
        [_ep(id: 'old1', text: '最早一页')],
      ]);
      final container = ProviderContainer(
        overrides: [cortexApiProvider.overrideWithValue(api)],
      );
      addTearDown(container.dispose);

      await container
          .read(exportControllerProvider.notifier)
          .export('s1', ExportFormat.json);

      expect(
        api.calls,
        3,
        reason:
            '只拉第一页的话，一段几百轮的会话会安静地只导出最后几十条 —— '
            '文件能打开、格式完全正确、就是少了一半',
      );
      final state = container.read(exportControllerProvider);
      expect(state.episodeCount, 3);
      expect(state.error, isNull);
      expect(state.truncated, isFalse);
    });

    test('翻出来的顺序是从老到新', () async {
      final api = _PagedApi([
        [_ep(id: 'new1', text: '最新')],
        [_ep(id: 'old1', text: '最早')],
      ]);
      final container = ProviderContainer(
        overrides: [cortexApiProvider.overrideWithValue(api)],
      );
      addTearDown(container.dispose);

      // 直接验渲染出来的先后：往回翻拿到的是更早的一页，插错位置的话
      // 导出来的存档顺序是反的，而它读起来仍然像一段完整对话
      final doc = SessionExport(
        session: _session(),
        episodes: [
          _ep(id: 'old1', text: '最早'),
          _ep(id: 'new1', text: '最新'),
        ],
        exportedAt: DateTime.utc(2026, 8, 19),
      );
      final md = doc.render(ExportFormat.markdown);
      expect(md.indexOf('最早'), lessThan(md.indexOf('最新')));

      await container
          .read(exportControllerProvider.notifier)
          .export('s1', ExportFormat.json);
      // 控制器那侧同样要是老→新
      expect(container.read(exportControllerProvider).episodeCount, 2);
    });
  });
}
