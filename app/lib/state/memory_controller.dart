import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/episode.dart';
import '../models/memory_search_result.dart';
import 'app_providers.dart';

class MemoryState {
  const MemoryState({
    this.query = '',
    this.result = MemorySearchResult.empty,
    this.loading = false,
    this.error,
    this.asOf,
    this.hasSearched = false,
  });

  final String query;
  final MemorySearchResult result;
  final bool loading;
  final String? error;

  /// Transaction-time replay point. Null = "now".
  final DateTime? asOf;

  final bool hasSearched;

  bool get isTimeTravelling => asOf != null;

  MemoryState copyWith({
    String? query,
    MemorySearchResult? result,
    bool? loading,
    Object? error = _sentinel,
    Object? asOf = _sentinel,
    bool? hasSearched,
  }) => MemoryState(
    query: query ?? this.query,
    result: result ?? this.result,
    loading: loading ?? this.loading,
    error: error == _sentinel ? this.error : error as String?,
    asOf: asOf == _sentinel ? this.asOf : asOf as DateTime?,
    hasSearched: hasSearched ?? this.hasSearched,
  );

  static const Object _sentinel = Object();
}

class MemoryController extends Notifier<MemoryState> {
  Timer? _debounce;

  /// Guards against a slow early request overwriting a fast later one.
  int _requestSeq = 0;

  @override
  MemoryState build() {
    ref.onDispose(() => _debounce?.cancel());
    // 后端换了就重来 —— mock 与 live 的结果没有可比性。
    //
    // # 这里为什么要**作废在飞的请求**，而不只是清空
    //
    // 换后端会 dispose 掉旧的 `HttpCortexApi`，而它的 `dispose` 是
    // `_client.close()` —— **正在飞的请求会当场被掐断**，抛出
    // 「Connection closed before full header was received」。
    //
    // 那个异常在清空之后才落地，于是它把 `error` 写了回去，界面停在
    // 「检索失败」直到用户手点一次刷新。而这恰恰是最常见的一次：
    // 应用刚起来时本地 agent 还没就绪，客户端先指向远端；一两秒后
    // agent 起好、provider 重建 —— 每次冷启动都命中。
    //
    // 作废序号让那具尸体被当作「已被取代」丢掉（`search` 里那道
    // `seq != _requestSeq` 的闸门），清空才真的是清空。
    ref.listen(cortexApiProvider, (_, _) {
      _requestSeq++;
      final searched = state.hasSearched;
      state = const MemoryState();
      // 之前搜过就自动重来一次：用户没有做错任何事，不该由他来点刷新。
      // 没搜过就什么也不做 —— 那时面板可能根本没打开
      if (searched) unawaited(search());
    });
    return const MemoryState();
  }

  void setQuery(String query) {
    state = state.copyWith(query: query);
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 260), search);
  }

  /// Moves the replay point. Re-runs immediately: the user is deliberately
  /// comparing two instants, so debouncing here would feel broken.
  void setAsOf(DateTime? asOf) {
    state = state.copyWith(asOf: asOf);
    search();
  }

  /// [Ref.mounted] is checked alongside the sequence guard because the caller
  /// is no longer only the user: `SyncController` re-runs this from a WebSocket
  /// bump, at a moment that has nothing to do with this provider's lifetime.
  /// Writing state after disposal throws out of an unawaited future.
  Future<void> search() async {
    _debounce?.cancel();
    if (!ref.mounted) return;
    final seq = ++_requestSeq;
    final query = state.query.trim();

    state = state.copyWith(loading: true, error: null, hasSearched: true);
    try {
      final result = await ref
          .read(cortexApiProvider)
          .searchMemory(query, limit: 30, asOf: state.asOf);
      if (seq != _requestSeq || !ref.mounted) return; // superseded
      state = state.copyWith(result: result, loading: false);
    } on CortexApiException catch (e) {
      if (seq != _requestSeq || !ref.mounted) return;
      state = state.copyWith(loading: false, error: e.message);
    } on Object catch (e) {
      if (seq != _requestSeq || !ref.mounted) return;
      state = state.copyWith(loading: false, error: '$e');
    }
  }
}

final memoryControllerProvider =
    NotifierProvider<MemoryController, MemoryState>(MemoryController.new);

/// Provenance lookup for a single fact's source episode.
///
/// A family provider rather than an imperative fetch so several fact cards can
/// each open their own episode and Riverpod handles the caching/dedup.
final episodeProvider = FutureProvider.family<Episode, String>((ref, id) async {
  return ref.watch(cortexApiProvider).episode(id);
});
