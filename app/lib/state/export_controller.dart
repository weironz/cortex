import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../core/save_file.dart';
import '../core/session_export.dart';
import '../models/episode.dart';
import 'app_providers.dart';

/// 「另存为」那一步。
///
/// # 为什么把它做成一个可替换的 provider
///
/// 不是抽象洁癖：它是**一条平台通道**。纯 `test()` 里碰它会抛
/// `MissingPluginException`，而那个异常会被 [ExportController.export] 的
/// catch 吞成一句 `state.error` —— 于是一组测「翻页」的测试挂在保存上，
/// 读失败信息（「No implementation found for method save」）完全看不出
/// 跟翻页有没有关系。2026-08-23 就是这么排了一轮。
///
/// 换成一个接缝之后，测翻页的只管翻页，测保存的自己去替换它。
typedef SaveBytes = Future<String?> Function(Uint8List bytes, String filename);

final saveBytesProvider = Provider<SaveBytes>(
  (_) =>
      (bytes, filename) => saveBytesAs(bytes, filename),
);

/// 一次导出最多往回翻多少页。
///
/// **不是为了限制导出的长度**，是为了在游标出问题时不无限翻下去 ——
/// 一个永远回 `has_more: true` 的服务端会让这个循环把内存吃光。
/// 每页 500 条（服务端上限），200 页 = 十万条消息，远超任何真实会话。
const _maxPages = 200;

/// 每页拉多少。用服务端允许的上限：导出是一次性动作，往返次数越少越好。
const _pageSize = 500;

/// 导出会话。
///
/// # 为什么要自己翻页，而不是用屏幕上那份
///
/// 界面里的 transcript 只有**最新那一页**（往上滚才会加载更早的）。
/// 直接导出它，一段几百轮的会话会安静地只导出最后几十条 —— 文件能打开、
/// 格式完全正确、就是少了一半。用户不会发现，直到他去找某句话找不到。
///
/// 所以这里从 `GET /sessions/{id}` 一路往回翻到底，与屏幕上加载了多少无关。
class ExportController extends Notifier<ExportState> {
  CortexApi get _api => ref.read(cortexApiProvider);

  @override
  ExportState build() => const ExportState();

  /// 导出一个会话并交给系统的「另存为」。
  ///
  /// 返回是否真的存下来了 —— `false` 可能是失败，也可能是用户自己取消了，
  /// 两者由 [ExportState.error] 区分（取消时它是 null）。
  Future<bool> export(String sessionId, ExportFormat format) async {
    if (state.busy) return false;
    state = const ExportState(busy: true);
    try {
      final first = await _api.sessionDetail(sessionId, limit: _pageSize);
      // 从老到新：每一页内部已经是正序，往回翻拿到的是更早的一页，
      // 所以新页要插在**前面**
      final all = <Episode>[...first.episodes];
      var cursor = first.hasMore ? first.nextCursor : null;
      var pages = 1;
      while (cursor != null && pages < _maxPages) {
        final page = await _api.sessionDetail(
          sessionId,
          limit: _pageSize,
          before: cursor,
        );
        all.insertAll(0, page.episodes);
        cursor = page.hasMore ? page.nextCursor : null;
        pages++;
      }
      // 翻满了还没到头 —— 说出来。悄悄导一份不完整的存档，比导不出来更糟
      final truncated = cursor != null;

      final doc = SessionExport(
        session: first.session,
        episodes: all,
        exportedAt: DateTime.now(),
      );
      final bytes = Uint8List.fromList(utf8.encode(doc.render(format)));
      // `saveBytesAs` 现在回**真实路径**（Web 上回文件名）。此前它回 bool，
      // 界面只能拿本地拼的文件名充数 —— 而那时用户根本没选过位置
      final saved = await ref.read(saveBytesProvider)(
        bytes,
        doc.fileName(format),
      );
      state = ExportState(
        savedName: saved,
        episodeCount: all.length,
        truncated: truncated,
      );
      return saved != null;
    } on CortexApiException catch (e) {
      state = ExportState(error: e.message);
      return false;
    } on Object catch (e) {
      state = ExportState(error: '$e');
      return false;
    }
  }
}

class ExportState {
  const ExportState({
    this.busy = false,
    this.error,
    this.savedName,
    this.episodeCount = 0,
    this.truncated = false,
  });

  final bool busy;
  final String? error;

  /// 存下来的文件名。`null` = 还没存（或者用户取消了）。
  final String? savedName;

  final int episodeCount;

  /// 翻到了页数上限还没到头，这份存档**不完整**。
  final bool truncated;
}

final exportControllerProvider =
    NotifierProvider<ExportController, ExportState>(ExportController.new);
