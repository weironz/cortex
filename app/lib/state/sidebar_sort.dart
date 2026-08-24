/// 左栏的**排序方式** —— 与「分不分组」是两件互不排斥的事。
///
/// 设计稿把菜单切成两组正是这个意思：「整理侧边栏」（按项目分组 /
/// 摊成一个列表）管**结构**，「排序方式」三档管**顺序** —— 可以
/// 「按项目分组 + 手动排序」。所以这里只管顺序，一个字都不碰分组。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../models/chat_session.dart';
import 'app_providers.dart';

enum SidebarSort {
  /// 最近更新在前。**默认** —— 与「我刚才在弄哪个」最贴近。
  recent('最近更新'),

  /// 按「谁在等我」排：等你确认 → 跑完没看 → 在跑 → 置顶 → 其余按时间。
  ///
  /// # 为什么这一档叫「优先级」而不是「按状态」
  ///
  /// 它排的不是状态本身，是**下一步该看哪个**。四种状态里只有前两种
  /// 需要人动手（确认、看结果），「在跑」排第三是因为它不需要你做什么、
  /// 但你可能想盯着。这个顺序就是这一档存在的全部理由 —— 换成字母序
  /// 或者别的什么，它就只是又一个排序而已。
  priority('优先级'),

  /// 你自己拖出来的顺序。
  ///
  /// # 为什么顺序存在本地而不是服务端
  ///
  /// 服务端的会话表没有 order 列，加一列意味着一条同步路径与一次迁移。
  /// 而「我习惯把这个放最上面」是**这台设备上的**习惯：同一个人在公司
  /// 那台机器上的排法，未必是他在家想要的。与左栏折叠状态同一个判据，
  /// 存法也一样（`settings.json` 里一个键）。
  manual('手动排序');

  const SidebarSort(this.label);

  final String label;

  static SidebarSort fromWire(String? s) =>
      SidebarSort.values.where((v) => v.name == s).firstOrNull ??
      SidebarSort.recent;
}

/// 排序方式 + 手动那一档的顺序表，跨重启记住。
class SidebarSortState {
  const SidebarSortState({this.mode = SidebarSort.recent, this.order = const []});

  final SidebarSort mode;

  /// 手动档的会话 id 顺序。**不在这张表里的排在后面** ——
  /// 新建的会话因此自动落到底部之前（见 `sortSessions`），
  /// 而不是消失或者被塞到一个随机位置。
  final List<String> order;

  SidebarSortState copyWith({SidebarSort? mode, List<String>? order}) =>
      SidebarSortState(mode: mode ?? this.mode, order: order ?? this.order);
}

class SidebarSortNotifier extends Notifier<SidebarSortState> {
  static const String _modeKey = 'sidebar_sort_mode';
  static const String _orderKey = 'sidebar_sort_order';

  @override
  SidebarSortState build() {
    Future.microtask(_restore);
    return const SidebarSortState();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final order = (saved[_orderKey] ?? '')
        .split(',')
        .map((s) => s.trim())
        .where((s) => s.isNotEmpty)
        .toList(growable: false);
    state = SidebarSortState(
      mode: SidebarSort.fromWire(saved[_modeKey]),
      order: order,
    );
  }

  void setMode(SidebarSort mode) {
    if (state.mode == mode) return;
    state = state.copyWith(mode: mode);
    ref.read(settingsPatcherProvider)(_modeKey, mode.name);
  }

  /// 把 [id] 挪到 [targetIndex] 之前（拖放落点）。
  ///
  /// **只在手动档有意义**，但不在这里拦：拦的话调用方要各判一次，
  /// 而界面本来就只在那一档画拖拽把手。
  void reorder(List<String> visibleIds, String id, int targetIndex) {
    final next = [...visibleIds]..remove(id);
    next.insert(targetIndex.clamp(0, next.length), id);
    state = state.copyWith(order: next);
    ref.read(settingsPatcherProvider)(_orderKey, next.join(','));
  }
}

final sidebarSortProvider =
    NotifierProvider<SidebarSortNotifier, SidebarSortState>(
      SidebarSortNotifier.new,
    );

/// 一条会话此刻有多「需要你」。小的排前面。
///
/// 抽成纯函数是为了能单测：真正的调用点在一个要 provider 的排序里，
/// 而这个顺序**就是「优先级」那一档的全部内容** —— 它错了的话，
/// 那一档与「最近更新」看起来没有区别。
int priorityRank({
  required bool awaitingConfirm,
  required bool justFinished,
  required bool streaming,
  required bool pinned,
}) {
  if (awaitingConfirm) return 0; // 唯一「不管就永远卡住」的
  if (justFinished) return 1; // 跑完了你还没看
  if (streaming) return 2; // 在跑，不用你动手
  if (pinned) return 3; // 你自己钉上去的
  return 4;
}

/// 按当前排序方式排一遍。**纯函数**，同上。
List<ChatSession> sortSessions(
  List<ChatSession> sessions,
  SidebarSortState sort, {
  required Set<String> awaiting,
  required Set<String> finished,
  required Set<String> running,
}) {
  final out = [...sessions];
  switch (sort.mode) {
    case SidebarSort.recent:
      // 服务端已经是这个序 —— 不重排，免得两处排法有细微差别时
      // 列表在「刚拉回来」与「刷新后」之间跳一下
      break;
    case SidebarSort.priority:
      out.sort((a, b) {
        final ra = priorityRank(
          awaitingConfirm: awaiting.contains(a.id),
          justFinished: finished.contains(a.id),
          streaming: running.contains(a.id),
          pinned: a.pinned,
        );
        final rb = priorityRank(
          awaitingConfirm: awaiting.contains(b.id),
          justFinished: finished.contains(b.id),
          streaming: running.contains(b.id),
          pinned: b.pinned,
        );
        // 同档内保持服务端那个「最近更新在前」的相对顺序 ——
        // `List.sort` 不保证稳定，所以拿 id 兜底（ULID 字典序 = 时序倒过来）
        return ra != rb ? ra.compareTo(rb) : b.id.compareTo(a.id);
      });
    case SidebarSort.manual:
      final rank = {for (var i = 0; i < sort.order.length; i++) sort.order[i]: i};
      out.sort((a, b) {
        // 没在顺序表里的排后面（新建的会话就是这种），彼此之间仍按时序
        final ra = rank[a.id] ?? 1 << 30;
        final rb = rank[b.id] ?? 1 << 30;
        return ra != rb ? ra.compareTo(rb) : b.id.compareTo(a.id);
      });
  }
  return out;
}
