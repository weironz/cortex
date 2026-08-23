/// 左栏下半部那三段的折叠状态。
///
/// # 为什么单独一个文件，而不是塞进 `app_providers.dart`
///
/// 那个文件已经装着「我在哪一页」「侧栏收没收起」「右栏画谁」三套布局状态，
/// 再加一套只有会话列表用得上的东西，等于让每个只想读 `mainView` 的人
/// 都把它一起编进来。
///
/// # 为什么跨重启记住
///
/// 与侧栏收起同源：一个把「聊天」折起来只看项目的人，多半要这么待一阵子。
/// 每次开窗都全部展开，等于这个折叠只在当前这个窗口里有效。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app_providers.dart';

/// 左栏下半部的三段。
///
/// **是枚举不是字符串**：段名要同时出现在「折叠状态的键」「段头的标题」
/// 与「测试里的抓手」三处，散成字面量的话改一处就会有两处对不上。
enum SidebarSection {
  /// 置顶的**项目**，展开是它下面的会话。
  projects('projects', '项目', '把项目置顶，它会出现在这里'),

  /// 置顶的**会话**，平铺。
  pinned('pinned', 'Pinned', '把对话置顶，它会出现在这里'),

  /// 其余会话。未置顶的项目仍然在这一段里以分组标题出现。
  chats('chats', '聊天', '还没有对话');

  const SidebarSection(this.key, this.label, this.emptyHint);

  /// 存进设置时用的键的后半段。**与 [name] 分开**：枚举名是 Dart 侧的
  /// 标识符，会随重构改；这个是落盘的格式，改了就是所有人的折叠状态清零。
  final String key;

  final String label;

  /// 这一段空着时，段头底下那一行字。
  ///
  /// # 为什么它必须存在
  ///
  /// 空段原先整个不画，理由是「常年空着的标题只是噪音」。但置顶会话与
  /// 置顶项目这两个功能在界面上**只有段头这一个入口** —— 不画段头，
  /// 用户就不知道能置顶，于是永远没有置顶，于是永远不画。
  /// 2026-08-23 用户报「左侧只有一个聊天的分组」，正是这个闭环。
  ///
  /// # 为什么跟着枚举走
  ///
  /// 与 [label] 同一个理由：它是这一段的属性，散成 widget 里的字面量之后，
  /// 加第四段的人会漏掉其中一处，而漏掉的表现是一段空着什么都不说。
  final String emptyHint;
}

/// 哪几段是折起来的。
class SidebarSectionsNotifier extends Notifier<Set<SidebarSection>> {
  static const String _key = 'sidebar_collapsed_sections';

  @override
  Set<SidebarSection> build() {
    Future.microtask(_restore);
    // 默认全展开：一个新用户第一眼就该看见自己有什么
    return const {};
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final keys = (saved[_key] ?? '').split(',').map((s) => s.trim()).toSet();
    final restored = SidebarSection.values
        .where((s) => keys.contains(s.key))
        .toSet();
    if (restored.isNotEmpty) state = restored;
  }

  bool isCollapsed(SidebarSection section) => state.contains(section);

  void toggle(SidebarSection section) {
    final next = {...state};
    if (!next.remove(section)) next.add(section);
    state = next;
    // 存的是**折起来的那些**，不是展开的那些：默认全展开，于是空串就是
    // 默认态 —— 反过来存的话，一个还没存过任何东西的新用户会被读成
    // 「三段全折起来」，界面一片空白
    ref.read(settingsPatcherProvider)(_key, next.map((s) => s.key).join(','));
  }
}

final sidebarSectionsProvider =
    NotifierProvider<SidebarSectionsNotifier, Set<SidebarSection>>(
      SidebarSectionsNotifier.new,
    );
