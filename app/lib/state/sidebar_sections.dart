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
  projects('projects', '项目'),

  /// 置顶的**会话**，平铺。
  pinned('pinned', 'Pinned'),

  /// 其余会话。未置顶的项目仍然在这一段里以分组标题出现。
  chats('chats', '聊天');

  const SidebarSection(this.key, this.label);

  /// 存进设置时用的键的后半段。**与 [name] 分开**：枚举名是 Dart 侧的
  /// 标识符，会随重构改；这个是落盘的格式，改了就是所有人的折叠状态清零。
  final String key;

  final String label;
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
