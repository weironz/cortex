/// 资料库的状态 —— 一页材料、当前文件夹、当前页签。
///
/// 与 `image_controller` 形状刻意一致（同样的翻页、同样的多选、同样的
/// 「换文件夹是重取不是过滤」）：两页并排放在左栏里，行为不一样的话
/// 用户要为同一个动作记两套结果。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../models/generated_image.dart';
import '../models/library_item.dart';
import 'app_providers.dart';

/// 三个页签。设计稿那三个：全部 / 图片 / 文件。
enum LibraryTab {
  all('全部'),
  images('图片'),
  files('文件');

  const LibraryTab(this.label);

  final String label;

  /// 线上写法。与服务端 `ListQuery.tab` 那三个字面量一一对应 ——
  /// 拼错的症状是筛选静默失效（服务端认不出就当 all）
  String get wire => name;
}

class LibraryState {
  const LibraryState({
    this.items = const [],
    this.folders = const [],
    this.tab = LibraryTab.all,
    this.folder,
    this.loading = false,
    this.error,
    this.unsupported = false,
    this.selected = const {},
  });

  final List<LibraryItem> items;
  final List<Folder> folders;
  final LibraryTab tab;

  /// 当前只看哪个文件夹。`null` = 全部；`"none"` = 只看未归档。
  final String? folder;

  final bool loading;
  final String? error;

  /// 这个部署没有资料库（老服务端 / 没接数据库）。
  ///
  /// 与 `error` 分开：**这不是故障**，界面要说「这个部署没有资料库」
  /// 并且不给重试按钮 —— 重试一百次也不会有。
  final bool unsupported;

  final Set<String> selected;

  /// 未归档的那些（界面上单独一段）。
  List<LibraryItem> get unfiled =>
      items.where((i) => i.folderId == null).toList(growable: false);

  LibraryState copyWith({
    List<LibraryItem>? items,
    List<Folder>? folders,
    LibraryTab? tab,
    Object? folder = _sentinel,
    bool? loading,
    Object? error = _sentinel,
    bool? unsupported,
    Set<String>? selected,
  }) => LibraryState(
    items: items ?? this.items,
    folders: folders ?? this.folders,
    tab: tab ?? this.tab,
    folder: folder == _sentinel ? this.folder : folder as String?,
    loading: loading ?? this.loading,
    error: error == _sentinel ? this.error : error as String?,
    unsupported: unsupported ?? this.unsupported,
    selected: selected ?? this.selected,
  );

  static const Object _sentinel = Object();
}

class LibraryController extends Notifier<LibraryState> {
  @override
  LibraryState build() {
    // 换后端就换一份资料库 —— 与图片页同一个理由：上一个部署的材料
    // 留在屏幕上，用户会以为它们跟着账号走
    ref.listen(appConfigProvider, (previous, next) {
      if (previous?.baseUrl != next.baseUrl ||
          previous?.useMock != next.useMock) {
        Future.microtask(refresh);
      }
    });
    Future.microtask(refresh);
    return const LibraryState(loading: true);
  }

  Future<void> refresh() async {
    state = state.copyWith(loading: true, error: null);
    try {
      final page = await ref
          .read(cortexApiProvider)
          .library(folder: state.folder, tab: state.tab.wire);
      state = state.copyWith(
        items: page.items,
        folders: page.folders.map(Folder.fromJson).toList(growable: false),
        loading: false,
        error: null,
        unsupported: false,
        selected: const {},
      );
    } on Object catch (e) {
      // 501 = 这个部署没有资料库，**不是错误**（见 `unsupported` 那段）
      final is501 = '$e'.contains('501') || '$e'.contains('没有资料库');
      state = state.copyWith(
        loading: false,
        unsupported: is501,
        error: is501 ? null : '$e',
      );
    }
  }

  /// 换页签。**重取，不是过滤手上这些** —— 只过滤这一页的话，
  /// 「文件」页会少掉所有还没翻到的，而它看起来是完整的。
  Future<void> setTab(LibraryTab tab) async {
    if (state.tab == tab) return;
    state = state.copyWith(tab: tab, selected: const {});
    await refresh();
  }

  /// 换文件夹。同上，重取。
  Future<void> setFolder(String? folder) async {
    if (state.folder == folder) return;
    state = state.copyWith(folder: folder, selected: const {});
    await refresh();
  }

  void toggleSelect(String id) {
    final next = {...state.selected};
    if (!next.remove(id)) next.add(id);
    state = state.copyWith(selected: next);
  }

  void clearSelection() => state = state.copyWith(selected: const {});

  /// 把勾中的移进一个文件夹（`null` = 移出来）。
  Future<String> moveSelectedTo(String? folderId, String folderName) async {
    final ids = state.selected.toList();
    if (ids.isEmpty) return '';
    final api = ref.read(cortexApiProvider);
    var failed = 0;
    for (final id in ids) {
      try {
        await api.updateLibraryItem(id, folderId: folderId, moveFolder: true);
      } on Object {
        failed++;
      }
    }
    await refresh();
    if (failed > 0) return '移动了 ${ids.length - failed} 项，另有 $failed 项没成功。';
    return folderId == null
        ? '已移出文件夹（${ids.length} 项）。'
        : '已移进「$folderName」（${ids.length} 项）。';
  }

  /// 从资料库移除勾中的那些。**blob 不动**，对话里引用过的照常显示。
  Future<String> removeSelected() async {
    final ids = state.selected.toList();
    if (ids.isEmpty) return '';
    final api = ref.read(cortexApiProvider);
    var failed = 0;
    for (final id in ids) {
      try {
        await api.removeFromLibrary(id);
      } on Object {
        failed++;
      }
    }
    await refresh();
    return failed == 0
        ? '已从资料库移除 ${ids.length} 项。'
        : '移除了 ${ids.length - failed} 项，另有 $failed 项没成功。';
  }
}

final libraryControllerProvider =
    NotifierProvider<LibraryController, LibraryState>(LibraryController.new);
