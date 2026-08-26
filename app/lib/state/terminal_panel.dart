/// 终端面板：开着哪几个标签、哪个在前、要不要铺满。
///
/// # 为什么它不住在部件里
///
/// 三处从外面动它：顶栏那个终端按钮、Ctrl+` 快捷键、以及 `AppShell`
/// （它要按「铺没铺满」决定整块内容区怎么摆）。局部 state 够不着。
///
/// # 标签的生命周期跟着**会话**走
///
/// 每个标签背后是本机 agent 上一个真的 shell 子进程，它的 cwd 来自这个
/// 会话的工作区绑定。换到另一条会话，那些 shell 站在错的目录里 ——
/// 所以换会话就清空重来（部件随之销毁，WS 关闭，agent 那侧收掉子进程）。
///
/// **换页签、收起整栏都不清空**：那些 shell 里可能正跑着东西，而收起
/// 一栏是「我先不看」，不是「都别跑了」。部件靠 `Offstage` 留在树上
/// （见 `AppShell`），这一层只管簿子。
///
/// # 上限：没有
///
/// 服务端那道「每会话最多一个」2026-08-26 取消了（见
/// `cortex-local/src/terminal.rs` 的模块头）。这里也不设 —— 两处各设一个
/// 上限，超限时的表现会是「界面让你点，服务端拒了你」，而错误信息在
/// WS 升级失败里，用户看不到。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../core/ulid.dart';
import 'app_providers.dart';
import 'chat_controller.dart';

/// 一个标签 = 一个 shell。
class TerminalTab {
  const TerminalTab({required this.id, required this.label});

  /// 部件的 key。**必须稳定且唯一** —— 用序号当 key 的话，关掉中间那个
  /// 之后后面的标签会集体前移，于是它们各自换了一个 shell，
  /// 而屏幕上看起来只是「关掉了一个」。
  final String id;

  /// 「终端 1」。序号只增不补：关掉 2 再新建给的是 3。
  ///
  /// 补空位（新建的还叫 2）在一个正跑着东西的终端旁边会读错人 ——
  /// 「刚才那个终端 2」和「现在这个终端 2」是两个 shell。
  final String label;
}

class TerminalPanelState {
  const TerminalPanelState({
    this.sessionId,
    this.tabs = const [],
    this.activeId,
    this.expanded = false,
    this.nextNumber = 1,
  });

  /// 这些标签属于哪条会话。换了就清空，见库文档。
  final String? sessionId;

  final List<TerminalTab> tabs;

  /// 哪个在前。`null` = 一个都没有。
  final String? activeId;

  /// 铺满左侧栏右边的全部（盖住对话与右栏）。
  final bool expanded;

  /// 下一个标签叫几号。
  final int nextNumber;

  int get activeIndex {
    final i = tabs.indexWhere((t) => t.id == activeId);
    // -1 会让 IndexedStack 直接抛。回 0 是安全的：tabs 空时整个面板
    // 走的是另一支（见 `TerminalPanel`）
    return i < 0 ? 0 : i;
  }

  TerminalPanelState copyWith({
    String? sessionId,
    List<TerminalTab>? tabs,
    String? activeId,
    bool? expanded,
    int? nextNumber,
    bool clearActive = false,
  }) => TerminalPanelState(
    sessionId: sessionId ?? this.sessionId,
    tabs: tabs ?? this.tabs,
    activeId: clearActive ? null : (activeId ?? this.activeId),
    expanded: expanded ?? this.expanded,
    nextNumber: nextNumber ?? this.nextNumber,
  );
}

class TerminalPanelNotifier extends Notifier<TerminalPanelState> {
  @override
  TerminalPanelState build() {
    // 换会话就清空。⚠️ 用 listen 而不是在读取处顺手判：判在读取处的话，
    // 「切走再切回来」中间那一段里簿子还挂着旧会话的标签，而任何一处
    // 在那一刻读到它都会去连一个错会话的 shell
    ref.listen(chatControllerProvider.select((s) => s.activeSessionId), (
      _,
      now,
    ) {
      if (now == state.sessionId) return;
      state = TerminalPanelState(sessionId: now);
    });
    return TerminalPanelState(
      sessionId: ref.read(chatControllerProvider).activeSessionId,
    );
  }

  /// 顶栏按钮 / Ctrl+` ：把终端叫出来。
  ///
  /// 已经开着就收起 —— 与右栏那个开关同一套语义（同一个按钮既是
  /// 「给我看」也是「不看了」）。
  void toggle() {
    final layout = ref.read(layoutProvider);
    if (layout.rightPanel == RightPanel.terminal) {
      ref.read(layoutProvider.notifier).closeRight();
      return;
    }
    if (state.tabs.isEmpty) addTab();
    ref.read(layoutProvider.notifier).showRight(RightPanel.terminal);
  }

  /// 至少要有一个 —— 窄屏那条路用它（那里的开合归 `Scaffold` 管，
  /// 不该由这一层去 `showRight`）。
  void ensureTab() {
    if (state.tabs.isEmpty) addTab();
  }

  /// 加号。
  void addTab() {
    final tab = TerminalTab(
      id: Ulid.generate(),
      label: '终端 ${state.nextNumber}',
    );
    state = state.copyWith(
      tabs: [...state.tabs, tab],
      activeId: tab.id,
      nextNumber: state.nextNumber + 1,
      sessionId: ref.read(chatControllerProvider).activeSessionId,
    );
  }

  void select(String id) {
    if (state.activeId == id) return;
    state = state.copyWith(activeId: id);
  }

  /// 标签上那个 ×：**收掉这一个 shell**。
  ///
  /// 关掉最后一个就把整栏也收起来：留一块写着「一个终端都没有」的空面板
  /// 占着三分之一屏，不如让它让位 —— 顶栏那个按钮随时把它叫回来。
  void close(String id) {
    final rest = state.tabs.where((t) => t.id != id).toList();
    if (rest.isEmpty) {
      state = state.copyWith(tabs: rest, clearActive: true, expanded: false);
      ref.read(layoutProvider.notifier).closeRight();
      return;
    }
    // 关掉的是当前这个，就接住它右边那个；它是最后一个就接左边。
    // 与浏览器标签页一致 —— 换成「总是回到第一个」会让连着关几个变成
    // 每关一次跳一次
    var next = state.activeId;
    if (id == state.activeId) {
      final was = state.tabs.indexWhere((t) => t.id == id);
      next = rest[was.clamp(0, rest.length - 1)].id;
    }
    state = state.copyWith(tabs: rest, activeId: next);
  }

  /// 面板右上角那个 ×：**只收起，不关 shell**。
  ///
  /// 与标签上那个 × 是两件事。里面可能正跑着 `npm run dev` ——
  /// 「我先不看」不该把它停掉。
  void hide() => ref.read(layoutProvider.notifier).closeRight();

  void toggleExpanded() => state = state.copyWith(expanded: !state.expanded);
}

final terminalPanelProvider =
    NotifierProvider<TerminalPanelNotifier, TerminalPanelState>(
      TerminalPanelNotifier.new,
    );
