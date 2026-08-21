/// 图片页的状态 —— 画一张、翻画廊。
///
/// # 为什么画廊是这个控制器管，而不是一个 `FutureProvider`
///
/// 它要**翻页**（往后累加，不是每次重取），还要在画完之后把新的插到最前面。
/// `FutureProvider` 表达得了「一次取一份」，表达不了「一份在长」——
/// 硬用的话每次翻页都要 invalidate + 重取全部，越翻越慢。
library;

import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/generated_image.dart';
import 'app_providers.dart';

/// 一页取多少。与服务端那侧的上限（60）留出余量 —— 缩略图是**逐张**
/// 取字节的，一页 60 张就是 60 个并发请求，第一屏反而更慢。
const int _kPageSize = 24;

class ImageState {
  const ImageState({
    this.items = const [],
    this.loading = false,
    this.loadingMore = false,
    this.generating = false,
    this.hasMore = false,
    this.cursor,
    this.error,
    this.unsupported = false,
  });

  final List<GeneratedImage> items;

  /// 第一页在路上。
  final bool loading;

  /// 往后翻的那一页在路上。**与 [loading] 分开** ——
  /// 合成一个的话，翻页时整面墙会闪成骨架屏，而用户明明已经在看内容了。
  final bool loadingMore;

  /// 正在画。这一段要几十秒，界面必须说清它在干活。
  final bool generating;

  final bool hasMore;
  final String? cursor;

  /// 最近一次失败。**画失败与翻页失败共用它是有意的**：
  /// 这一页同时只可能有一个在飞（画的时候输入框禁用）。
  final Object? error;

  /// 这个后端没有画廊（老服务端 404）。
  ///
  /// **不是错误**，是能力说明 —— 画成红字会让用户去修一个没坏的东西。
  final bool unsupported;

  ImageState copyWith({
    List<GeneratedImage>? items,
    bool? loading,
    bool? loadingMore,
    bool? generating,
    bool? hasMore,
    String? cursor,
    Object? error,
    bool clearError = false,
    bool? unsupported,
  }) => ImageState(
    items: items ?? this.items,
    loading: loading ?? this.loading,
    loadingMore: loadingMore ?? this.loadingMore,
    generating: generating ?? this.generating,
    hasMore: hasMore ?? this.hasMore,
    cursor: cursor ?? this.cursor,
    error: clearError ? null : (error ?? this.error),
    unsupported: unsupported ?? this.unsupported,
  );
}

class ImageController extends Notifier<ImageState> {
  @override
  ImageState build() {
    Future.microtask(refresh);
    return const ImageState(loading: true);
  }

  /// 重取第一页（丢掉已翻的）。
  Future<void> refresh() async {
    state = state.copyWith(loading: true, clearError: true);
    try {
      final page = await ref.read(cortexApiProvider).gallery(limit: _kPageSize);
      if (!ref.mounted) return;
      state = ImageState(
        items: page.items,
        hasMore: page.hasMore,
        cursor: page.nextCursor,
      );
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      // 老服务端没有这条路。**说「这个后端没有画廊」，不说「出错了」** ——
      // 后者会让人去查网络、去重试，而重试永远不会成功
      state = ImageState(
        unsupported: e.isUnsupported,
        error: e.isUnsupported ? null : e.message,
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = ImageState(error: e);
    }
  }

  /// 往后翻一页。
  Future<void> loadMore() async {
    final cursor = state.cursor;
    // 已经在翻、或者没得翻了就什么都不做 —— 滚动到底会**连着**触发它
    if (cursor == null || !state.hasMore || state.loadingMore) return;
    state = state.copyWith(loadingMore: true, clearError: true);
    try {
      final page = await ref
          .read(cortexApiProvider)
          .gallery(limit: _kPageSize, before: cursor);
      if (!ref.mounted) return;
      state = ImageState(
        items: [...state.items, ...page.items],
        hasMore: page.hasMore,
        cursor: page.nextCursor,
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(loadingMore: false, error: e);
    }
  }

  /// 画几张。
  ///
  /// 画完**重取第一页**而不是把返回的哈希拼进去：提示词、型号、时间这些
  /// 由画廊回答，本地再拼一份就是同一件事的第二个来源，而两份迟早不一致。
  ///
  /// 返回是否真的画出来了 —— 调用方据此决定要不要清空输入框。
  Future<bool> generate({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  }) async {
    if (state.generating) return false;
    state = state.copyWith(generating: true, clearError: true);
    try {
      final hashes = await ref
          .read(cortexApiProvider)
          .generateImages(
            prompt: prompt,
            model: model,
            source: source,
            size: size,
            n: n,
          );
      if (!ref.mounted) return false;
      // 200 但零张图。当成成功的话，界面会说「画好了」而墙上什么都没多
      if (hashes.isEmpty) {
        state = state.copyWith(generating: false, error: '服务端说生成成功，但一张图都没回来');
        return false;
      }
      state = state.copyWith(generating: false);
      await refresh();
      return true;
    } on CortexApiException catch (e) {
      if (!ref.mounted) return false;
      // 服务端的消息**原样显示**：它说得比我们能编的具体
      //（「这条来源没有开放 x」「上游 402 余额不足」）
      state = state.copyWith(generating: false, error: e.message);
      return false;
    } on Object catch (e) {
      if (!ref.mounted) return false;
      state = state.copyWith(generating: false, error: e);
      return false;
    }
  }
}

final imageControllerProvider = NotifierProvider<ImageController, ImageState>(
  ImageController.new,
);
