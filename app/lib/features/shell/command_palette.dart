/// ⌘K 命令面板 —— 一个输入框管四件事，前缀区分。
///
/// * 无前缀：搜会话（全文，走服务端 `sessionSearchProvider`）
/// * `>`：命令（新对话、打开设置、切主题……）
/// * `@`：文件（当前工作区，选中后放进输入框）
/// * `/`：技能（选中打开编辑器）
///
/// # 为什么是前缀而不是页签
///
/// 页签要先想「我要找的东西算哪一类」再点一下；前缀是打字的自然延伸 ——
/// VS Code / Raycast 用了十年的形状，肌肉记忆是现成的。空输入时把四种
/// 前缀写在眼前，第一次用的人不必先学。
///
/// # 键盘契约
///
/// ↑↓ 选、Enter 执行、Esc 关。**鼠标悬停不改选中** —— 手在键盘上时，
/// 一次无意的鼠标掠过把选中换掉，Enter 就执行了别的东西。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../state/composer_draft.dart';
import '../../state/file_mention_controller.dart';
import '../../state/session_search_controller.dart';
import '../../state/skill_controller.dart';
import '../settings/pages/skills_page.dart';
import '../settings/settings_sheet.dart';

Future<void> showCommandPalette(BuildContext context) => showDialog<void>(
  context: context,
  // 面板贴上沿：它是「呼出来敲两下就走」的东西，居中会让视线来回跳
  builder: (_) =>
      const Align(alignment: Alignment(0, -0.72), child: _Palette()),
);

/// 一条可执行的结果。四种来源都归一成它 —— 列表与键盘逻辑只写一份。
class _Entry {
  const _Entry({
    required this.icon,
    required this.title,
    this.subtitle,
    required this.run,
  });

  final IconData icon;
  final String title;
  final String? subtitle;
  final void Function(BuildContext context, WidgetRef ref) run;
}

class _Palette extends ConsumerStatefulWidget {
  const _Palette();

  @override
  ConsumerState<_Palette> createState() => _PaletteState();
}

class _PaletteState extends ConsumerState<_Palette> {
  final TextEditingController _controller = TextEditingController();
  int _selected = 0;

  @override
  void initState() {
    super.initState();
    _controller.addListener(() {
      setState(() => _selected = 0);
      final text = _controller.text;
      // 无前缀才喂给会话搜索 —— 前缀模式下把 `>set` 发去搜全文，
      // 结果闪一下又消失，读起来像面板在抽搐
      if (!text.startsWith('>') &&
          !text.startsWith('@') &&
          !text.startsWith('/')) {
        ref.read(sessionSearchProvider.notifier).setQuery(text);
      }
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    ref.read(sessionSearchProvider.notifier).clear();
    super.dispose();
  }

  /// 静态命令表。动作里**先关面板再干活** —— 反过来的话，打开设置那类
  /// 压新路由的命令会把面板留在返回栈里，回来时它还开着。
  static final List<_Entry> _commands = [
    _Entry(
      icon: Icons.edit_outlined,
      title: '新对话',
      run: (context, ref) {
        ref.read(chatControllerProvider.notifier).startNewChat();
        ref.read(mainViewProvider.notifier).go(MainView.chat);
      },
    ),
    _Entry(
      icon: Icons.folder_outlined,
      title: '打开项目页',
      run: (context, ref) =>
          ref.read(mainViewProvider.notifier).go(MainView.projects),
    ),
    _Entry(
      icon: Icons.smart_toy_outlined,
      title: '打开智能体页',
      run: (context, ref) =>
          ref.read(mainViewProvider.notifier).go(MainView.assistants),
    ),
    _Entry(
      icon: Icons.image_outlined,
      title: '打开图片页',
      run: (context, ref) =>
          ref.read(mainViewProvider.notifier).go(MainView.images),
    ),
    _Entry(
      icon: Icons.settings_outlined,
      title: '打开设置',
      run: (context, ref) => showSettingsSheet(context),
    ),
    _Entry(
      icon: Icons.brightness_6_outlined,
      title: '切换主题',
      subtitle: '浅色 / 深色 / 跟随系统 循环',
      run: (context, ref) => ref.read(themeModeProvider.notifier).cycle(),
    ),
    _Entry(
      icon: Icons.view_sidebar_outlined,
      title: '收起 / 展开左栏',
      run: (context, ref) => ref.read(layoutProvider.notifier).toggleLeft(),
    ),
  ];

  List<_Entry> _entries() {
    final text = _controller.text;
    if (text.startsWith('>')) {
      final q = text.substring(1).trim().toLowerCase();
      return [
        for (final c in _commands)
          if (q.isEmpty || c.title.toLowerCase().contains(q)) c,
      ];
    }
    if (text.startsWith('@')) {
      final q = text.substring(1).trim();
      final files = ref.watch(fileMentionProvider).filter(q).take(8);
      return [
        for (final path in files)
          _Entry(
            icon: Icons.description_outlined,
            title: path,
            subtitle: '放进输入框',
            run: (context, ref) {
              ref.read(composerDraftProvider.notifier).offer('@$path ');
              ref.read(mainViewProvider.notifier).go(MainView.chat);
            },
          ),
      ];
    }
    if (text.startsWith('/')) {
      final q = text.substring(1).trim().toLowerCase();
      final skills = ref.watch(skillControllerProvider).skills;
      return [
        for (final s in skills)
          if (q.isEmpty || s.name.toLowerCase().contains(q))
            _Entry(
              icon: Icons.auto_stories_outlined,
              title: s.name,
              subtitle: s.enabled
                  ? (s.description.isEmpty ? null : s.description)
                  : '已停用',
              run: (context, ref) => showSkillEditor(context, ref, s),
            ),
      ];
    }
    // 无前缀：会话搜索
    final hits = ref.watch(sessionSearchProvider).hits;
    return [
      for (final h in hits)
        _Entry(
          icon: Icons.forum_outlined,
          title: h.title ?? '（无标题）',
          subtitle: h.excerpt,
          run: (context, ref) {
            ref
                .read(chatControllerProvider.notifier)
                .selectSession(h.sessionId);
            ref.read(mainViewProvider.notifier).go(MainView.chat);
          },
        ),
    ];
  }

  void _runSelected(List<_Entry> entries) {
    if (entries.isEmpty) return;
    final entry = entries[_selected.clamp(0, entries.length - 1)];
    // 先关面板再执行（见 _commands 上那段）
    Navigator.of(context).pop();
    entry.run(context, ref);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;
    final entries = _entries();
    final searching = ref.watch(sessionSearchProvider).loading;

    return Material(
      color: Colors.transparent,
      child: Container(
        width: 560,
        constraints: const BoxConstraints(maxHeight: 420),
        decoration: BoxDecoration(
          color: scheme.surface,
          borderRadius: BorderRadius.circular(CortexTokens.radiusWindow),
          border: Border.all(color: scheme.outline),
          boxShadow: kElevationToShadow[12],
        ),
        clipBehavior: Clip.antiAlias,
        child: Shortcuts(
          shortcuts: const {
            SingleActivator(LogicalKeyboardKey.arrowDown): _MoveIntent(1),
            SingleActivator(LogicalKeyboardKey.arrowUp): _MoveIntent(-1),
            SingleActivator(LogicalKeyboardKey.enter): _RunIntent(),
          },
          child: Actions(
            actions: {
              _MoveIntent: CallbackAction<_MoveIntent>(
                onInvoke: (i) {
                  if (entries.isNotEmpty) {
                    setState(
                      () =>
                          _selected = (_selected + i.delta) % entries.length < 0
                          ? entries.length - 1
                          : (_selected + i.delta) % entries.length,
                    );
                  }
                  return null;
                },
              ),
              _RunIntent: CallbackAction<_RunIntent>(
                onInvoke: (_) {
                  _runSelected(entries);
                  return null;
                },
              ),
            },
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(14, 10, 14, 8),
                  child: TextField(
                    controller: _controller,
                    autofocus: true,
                    decoration: const InputDecoration(
                      hintText: '搜会话，或  > 命令   @ 文件   / 技能',
                      prefixIcon: Icon(Icons.search_rounded, size: 18),
                    ),
                  ),
                ),
                Divider(height: 1, color: tokens.sidebarBorder),
                Flexible(
                  child: entries.isEmpty
                      ? Padding(
                          padding: const EdgeInsets.all(20),
                          child: Text(
                            searching
                                ? '搜索中…'
                                : _controller.text.isEmpty
                                ? '直接打字搜会话；> 命令、@ 文件、/ 技能。'
                                : '没有匹配的结果。',
                            style: theme.textTheme.bodySmall?.copyWith(
                              color: tokens.foregroundTertiary,
                            ),
                          ),
                        )
                      : ListView.builder(
                          shrinkWrap: true,
                          padding: const EdgeInsets.symmetric(vertical: 6),
                          itemCount: entries.length,
                          itemBuilder: (context, i) {
                            final e = entries[i];
                            final selected = i == _selected;
                            return InkWell(
                              onTap: () {
                                _selected = i;
                                _runSelected(entries);
                              },
                              child: Container(
                                color: selected
                                    ? tokens.sidebarAccent
                                    : Colors.transparent,
                                padding: const EdgeInsets.symmetric(
                                  horizontal: 16,
                                  vertical: 8,
                                ),
                                child: Row(
                                  children: [
                                    Icon(
                                      e.icon,
                                      size: 16,
                                      color: tokens.foregroundTertiary,
                                    ),
                                    const SizedBox(width: 10),
                                    Expanded(
                                      child: Column(
                                        crossAxisAlignment:
                                            CrossAxisAlignment.start,
                                        children: [
                                          Text(
                                            e.title,
                                            maxLines: 1,
                                            overflow: TextOverflow.ellipsis,
                                            style: theme.textTheme.bodyMedium,
                                          ),
                                          if (e.subtitle != null)
                                            Text(
                                              e.subtitle!,
                                              maxLines: 1,
                                              overflow: TextOverflow.ellipsis,
                                              style: theme.textTheme.labelSmall
                                                  ?.copyWith(
                                                    color: tokens
                                                        .foregroundTertiary,
                                                  ),
                                            ),
                                        ],
                                      ),
                                    ),
                                  ],
                                ),
                              ),
                            );
                          },
                        ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _MoveIntent extends Intent {
  const _MoveIntent(this.delta);
  final int delta;
}

class _RunIntent extends Intent {
  const _RunIntent();
}
