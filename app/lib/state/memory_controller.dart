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
    // Reset when the backend flips — mock and live results are unrelated.
    ref.listen(cortexApiProvider, (_, _) {
      state = const MemoryState();
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

  Future<void> search() async {
    _debounce?.cancel();
    final seq = ++_requestSeq;
    final query = state.query.trim();

    state = state.copyWith(loading: true, error: null, hasSearched: true);
    try {
      final result = await ref
          .read(cortexApiProvider)
          .searchMemory(query, limit: 30, asOf: state.asOf);
      if (seq != _requestSeq) return; // superseded
      state = state.copyWith(result: result, loading: false);
    } on CortexApiException catch (e) {
      if (seq != _requestSeq) return;
      state = state.copyWith(loading: false, error: e.message);
    } on Object catch (e) {
      if (seq != _requestSeq) return;
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
