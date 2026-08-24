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
    this.folder,
    this.selected = const {},
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

  /// 正在看哪个文件夹。`null` = 全部。
  ///
  /// **它是查询的一部分**：翻页游标是在这个条件下取出来的，换文件夹必须
  /// 从头拉。把它放进状态而不是页面里，正是为了让 [ImageController.loadMore]
  /// 也看得见 —— 页面持有一份的话，翻页会拿「全部」的游标去翻一个文件夹。
  final String? folder;

  /// 勾中的那些（图库那一行的 id）。空 = 不在多选态。
  final Set<String> selected;

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
    Set<String>? selected,
  }) => ImageState(
    items: items ?? this.items,
    loading: loading ?? this.loading,
    loadingMore: loadingMore ?? this.loadingMore,
    generating: generating ?? this.generating,
    hasMore: hasMore ?? this.hasMore,
    cursor: cursor ?? this.cursor,
    error: clearError ? null : (error ?? this.error),
    unsupported: unsupported ?? this.unsupported,
    folder: folder,
    selected: selected ?? this.selected,
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
      final folder = state.folder;
      final page = await ref
          .read(cortexApiProvider)
          .gallery(limit: _kPageSize, folder: folder);
      if (!ref.mounted) return;
      state = ImageState(
        items: page.items,
        hasMore: page.hasMore,
        cursor: page.nextCursor,
        folder: folder,
        // 勾选**跟着这一页作废**：留着的话，用户切到别的文件夹再点「加入文件夹」，
        // 加进去的是他现在根本看不见的那几张
        selected: const {},
      );
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      // 老服务端没有这条路。**说「这个后端没有画廊」，不说「出错了」** ——
      // 后者会让人去查网络、去重试，而重试永远不会成功
      state = ImageState(
        unsupported: e.isUnsupported,
        error: e.isUnsupported ? null : e.message,
        folder: state.folder,
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = ImageState(error: e, folder: state.folder);
    }
  }

  /// 换一个文件夹看（`null` = 全部）。
  ///
  /// **必须重取第一页**，不能只过滤手上这些：手上这些只是最新的一页，
  /// 过滤出来的「这个文件夹」会少掉所有还没翻到的 —— 而它看起来是完整的。
  Future<void> setFolder(String? folder) async {
    if (folder == state.folder) return;
    // ⚠️ 直接造一个新状态，不走 `copyWith` —— 它**不改 folder**（那是查询
    // 条件，不是可选覆盖），改了才会连着把游标一起换掉
    state = ImageState(loading: true, folder: folder);
    await refresh();
  }

  /// 勾 / 取消勾一张。
  void toggleSelect(String id) {
    final next = {...state.selected};
    if (!next.remove(id)) next.add(id);
    state = state.copyWith(selected: next);
  }

  void clearSelection() => state = state.copyWith(selected: const {});

  void selectAll() =>
      state = state.copyWith(selected: {for (final i in state.items) i.id});

  /// 把勾中的那些从图库移除。**blob 不动**，对话里那些照常显示。
  ///
  /// 一张一张地删而不是一条批量接口：这条路上出错的唯一真实原因是某一张
  /// 已经被别处删了，而那种时候「其余的照删」比「整批回滚」更接近用户想要的。
  /// 失败的张数如实报出来。
  Future<String> removeSelected() async {
    final ids = state.selected.toList();
    if (ids.isEmpty) return '';
    final api = ref.read(cortexApiProvider);
    var failed = 0;
    for (final id in ids) {
      try {
        await api.removeImage(id);
      } on Object {
        failed++;
      }
    }
    await refresh();
    return failed == 0
        ? '已从图库移除 ${ids.length} 张（对话里那些还在）。'
        : '移除了 ${ids.length - failed} 张，另有 $failed 张没成功。';
  }

  /// 把勾中的那些**移进**一个文件夹（`null` = 移出来，回未归档）。
  ///
  /// # 为什么没有「从文件夹里拿出」这个单独动作
  ///
  /// 归档排他之后，「移进 A」自带「离开原来那个」，而「拿出来」就是
  /// 「移进 null」—— 一个动作两个方向。相册时代那两个按钮（加进 / 拿出）
  /// 是多对多才需要的，留着会让人以为「移进 A」之后它还在 B 里。
  Future<String> moveSelectedTo(String? folderId, String folderName) async {
    final ids = state.selected.toList();
    if (ids.isEmpty) return '';
    final api = ref.read(cortexApiProvider);
    var failed = 0;
    for (final id in ids) {
      try {
        await api.moveImage(id, folderId);
      } on Object {
        failed++;
      }
    }
    // 张数变了，文件夹那一行上的数字得跟上
    ref.invalidate(foldersProvider);
    // 正在看某个文件夹时，移走的那些要从这一页消失 —— 不重取的话
    // 用户会看着自己刚移走的图还留在原地，以为没生效
    if (state.folder != null) {
      await refresh();
    } else {
      state = state.copyWith(selected: const {});
    }
    if (failed > 0) {
      return '移动了 ${ids.length - failed} 张，另有 $failed 张没成功。';
    }
    return folderId == null
        ? '已移出文件夹（${ids.length} 张）。'
        : '已移进「$folderName」（${ids.length} 张）。';
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
          .gallery(limit: _kPageSize, before: cursor, folder: state.folder);
      if (!ref.mounted) return;
      state = ImageState(
        items: [...state.items, ...page.items],
        hasMore: page.hasMore,
        cursor: page.nextCursor,
        folder: state.folder,
        selected: state.selected,
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

/// 文件夹列表。
///
/// # 为什么这个是 `FutureProvider`，而画廊不是
///
/// 文件夹**一次取完**（没有翻页），改动之后整份重取就够了 —— 服务端那几条
/// 写接口回的也正是整份列表。画廊不同，它是一份在长的东西（见本文件顶上）。
final foldersProvider = FutureProvider<Folders>((ref) async {
  try {
    return await ref.watch(cortexApiProvider).folders();
  } on CortexApiException catch (e) {
    // 老服务端没有 `/albums`。**回一份空的，不抛** —— 抛的话画廊上面
    // 会挂一条红字，而用户能做的只有升级服务端
    if (e.isUnsupported) return const Folders();
    rethrow;
  }
});
