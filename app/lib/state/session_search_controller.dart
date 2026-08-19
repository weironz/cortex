import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../models/session_search_hit.dart';
import 'app_providers.dart';

/// 侧栏搜索框的状态。
class SessionSearchState {
  const SessionSearchState({
    this.query = '',
    this.hits = const [],
    this.loading = false,
    this.error,
    this.unsupported = false,
  });

  /// 输入框里**此刻**的词 —— 不是发出去的那个。
  ///
  /// 界面按它决定「要不要显示搜索结果而不是会话列表」。跟着请求走的话，
  /// 用户敲下第一个字到防抖到期之间那 250 毫秒里，界面还画着完整的会话
  /// 列表 —— 看起来像输入被吞了。
  final String query;

  final List<SessionSearchHit> hits;
  final bool loading;
  final String? error;

  /// 这个后端没有 `/sessions/search`（老版本）。
  ///
  /// 与 [error] 分开，理由和 `ProjectState.unsupported` 一样：错误要显示、
  /// 要给重试；「没有这个功能」只该让搜索框安静地不存在。
  final bool unsupported;

  /// 现在该显示搜索结果，而不是那份会话列表。
  bool get active => query.trim().isNotEmpty;

  SessionSearchState copyWith({
    String? query,
    List<SessionSearchHit>? hits,
    bool? loading,
    Object? error = _sentinel,
    bool? unsupported,
  }) => SessionSearchState(
    query: query ?? this.query,
    hits: hits ?? this.hits,
    loading: loading ?? this.loading,
    error: error == _sentinel ? this.error : error as String?,
    unsupported: unsupported ?? this.unsupported,
  );

  static const Object _sentinel = Object();
}

/// 防抖窗口。
///
/// 中文输入法在一个词打完之前会连续触发 `onChanged`（拼音的每个字母都
/// 算一次），不防抖的话打「工作区」要往服务端发七八次全表 ILIKE。
/// 250 毫秒是打字停顿与「感觉不到延迟」之间那条线。
const _debounce = Duration(milliseconds: 250);

class SessionSearchController extends Notifier<SessionSearchState> {
  int _requestSeq = 0;
  Timer? _timer;

  bool _stale(int seq) => seq != _requestSeq || !ref.mounted;

  CortexApi get _api => ref.read(cortexApiProvider);

  @override
  SessionSearchState build() {
    // 换后端 = 换一整套会话。留着上一份结果的话，点进去是一个新后端上
    // 不存在的会话 id
    ref.listen(cortexApiProvider, (_, _) {
      _requestSeq++;
      _timer?.cancel();
      state = const SessionSearchState();
    });
    ref.onDispose(() => _timer?.cancel());
    return const SessionSearchState();
  }

  /// 输入框每敲一下走这里。
  void setQuery(String value) {
    _timer?.cancel();
    // 作废在飞的那次：用户又敲了一个字，上一次的结果已经不是他要的了。
    // 不作废的话，一次慢请求会在更晚的时候把旧词的结果盖回界面 ——
    // 本仓库反复咬人的「在飞请求不作废」
    _requestSeq++;
    final trimmed = value.trim();
    if (trimmed.isEmpty) {
      // 清空是**立刻**生效的，不走防抖：用户按了 Esc 或点了叉，
      // 他要的是马上看回会话列表，而不是 250 毫秒后
      state = const SessionSearchState();
      return;
    }
    state = state.copyWith(query: value, loading: true, error: null);
    final seq = _requestSeq;
    _timer = Timer(_debounce, () => unawaited(_run(trimmed, seq)));
  }

  /// 重试当前这个词。
  Future<void> retry() async {
    final trimmed = state.query.trim();
    if (trimmed.isEmpty) return;
    _timer?.cancel();
    _requestSeq++;
    state = state.copyWith(loading: true, error: null);
    await _run(trimmed, _requestSeq);
  }

  /// 清空 —— 关掉搜索，回到会话列表。
  void clear() => setQuery('');

  Future<void> _run(String query, int seq) async {
    try {
      final hits = await _api.searchSessions(query);
      if (_stale(seq)) return;
      state = state.copyWith(
        hits: hits,
        loading: false,
        error: null,
        unsupported: false,
      );
    } on CortexApiException catch (e) {
      if (_stale(seq)) return;
      state = e.isUnsupported
          ? state.copyWith(
              hits: const [],
              loading: false,
              error: null,
              unsupported: true,
            )
          : state.copyWith(loading: false, error: e.message);
    } on Object catch (e) {
      if (_stale(seq)) return;
      state = state.copyWith(loading: false, error: '$e');
    }
  }
}

final sessionSearchProvider =
    NotifierProvider<SessionSearchController, SessionSearchState>(
      SessionSearchController.new,
    );
