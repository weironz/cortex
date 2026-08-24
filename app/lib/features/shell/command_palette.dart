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
  final ScrollController _scroll = ScrollController();
  int _selected = 0;

  /// 已经为 `@` 分支踢过一次扫描。
  ///
  /// 去重不能交给 `ensure()` 自己：它只对「已经扫完」的会话短路，
  /// loading 期间再调会**再起一次全量扫描** —— 而 `@` 分支里每敲一个字
  /// 都会走到这里。面板是临时的，一次生命周期里踢一次正好。
  bool _mentionScanKicked = false;

  /// 每行固定高度。
  ///
  /// 固定而不是各排各的：itemExtent 让「第 N 行在视口哪儿」变成一道乘法，
  /// 键盘跟随滚动（[_revealSelected]）才算得准。52 = 上一版两行内容的
  /// 自然高度（8 + 标题 20 + 副标题 16 + 8），副标题行内截断、不撑高行。
  static const double _rowExtent = 52;

  /// 列表上下的内边距 —— 滚动数学要用到它，写死在两处迟早漂。
  static const double _listPad = 6;

  @override
  void initState() {
    super.initState();
    _controller.addListener(() {
      setState(() => _selected = 0);
      // 换了词选中项回到第一行，视口不跟着回去的话，高亮会停在看不见的地方
      if (_scroll.hasClients) _scroll.jumpTo(0);
      final text = _controller.text;
      // 面板要自己踢扫描 —— 唯一的另一处 ensure() 在输入框的 @ 弹层里，
      // 从 ⌘K 直接进 @ 分支的话没人扫过，列表会永远停在「没有匹配的文件」
      if (text.startsWith('@') && !_mentionScanKicked) {
        _mentionScanKicked = true;
        unawaited(ref.read(fileMentionProvider.notifier).ensure());
      }
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
    _scroll.dispose();
    ref.read(sessionSearchProvider.notifier).clear();
    super.dispose();
  }

  /// 把选中行滚进视口。
  ///
  /// ListView 自己不知道「选中」这回事：↑↓ 改的是 [_selected]，不动
  /// 滚动位置，于是选中项会移出视口、Enter 执行一条看不见的行。
  /// 行高固定（[_rowExtent]），位置是乘法，不必量 RenderObject。
  void _revealSelected() {
    if (!_scroll.hasClients) return;
    final pos = _scroll.position;
    final top = _listPad + _selected * _rowExtent;
    final bottom = top + _rowExtent;
    double? target;
    if (top - _listPad < pos.pixels) {
      target = top - _listPad;
    } else if (bottom + _listPad > pos.pixels + pos.viewportDimension) {
      target = bottom + _listPad - pos.viewportDimension;
    }
    if (target == null) return;
    _scroll.animateTo(
      target.clamp(0.0, pos.maxScrollExtent).toDouble(),
      duration: const Duration(milliseconds: 120),
      curve: Curves.easeOutCubic,
    );
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
          // 不带副标题 —— 八行同一句「放进输入框」是噪音，
          // 选中后做什么由 Enter 的行为自己说
          _Entry(
            icon: Icons.description_outlined,
            title: path,
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

  /// 列表空着时说什么。
  ///
  /// 四种来源各有各的「为什么是空的」：还在扫、没绑工作区、后端不支持、
  /// 网络错了 —— 一句笼统的「没有匹配的结果」会把它们全说成「没有」，
  /// 用户会以为要找的东西不存在。
  Widget _status(ThemeData theme) {
    final tokens = theme.cortex;
    final scheme = theme.colorScheme;
    final text = _controller.text;
    final dim = theme.textTheme.bodySmall?.copyWith(
      color: tokens.foregroundTertiary,
    );

    Widget line(String message) => Padding(
      padding: const EdgeInsets.all(20),
      child: Text(message, style: dim),
    );

    if (text.startsWith('@')) {
      final index = ref.watch(fileMentionProvider);
      if (!index.available) return line('这个会话没有绑定工作区');
      if (index.loading) return line('正在看工作区里有什么…');
      return line('没有匹配的文件');
    }
    if (text.startsWith('>') || text.startsWith('/')) {
      return line('没有匹配的结果。');
    }
    final search = ref.watch(sessionSearchProvider);
    if (search.error != null) {
      // 错误要显示、要给一条重试的路 —— 只说「没有匹配的结果」的话，
      // 一次网络抖动看起来就像「那条会话不存在」
      return Padding(
        padding: const EdgeInsets.fromLTRB(20, 10, 12, 10),
        child: Row(
          children: [
            Expanded(
              child: Text(
                search.error!,
                style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
              ),
            ),
            TextButton(
              onPressed: () => ref.read(sessionSearchProvider.notifier).retry(),
              child: const Text('重试'),
            ),
          ],
        ),
      );
    }
    // 「没有这个功能」与错误分开：它不该长得像坏了，也不该给重试
    if (search.unsupported) return line('这个部署不支持全文搜索');
    if (search.loading) return line('搜索中…');
    if (text.isEmpty) return line('直接打字搜会话；> 命令、@ 文件、/ 技能。');
    return line('没有匹配的结果。');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;
    final entries = _entries();
    // 只在 @ 分支 watch —— 脚注要用 truncated，别的分支不必因为
    // 扫描状态变化而重建
    final mention = _controller.text.startsWith('@')
        ? ref.watch(fileMentionProvider)
        : null;

    return Material(
      color: Colors.transparent,
      child: Container(
        width: 560,
        constraints: const BoxConstraints(maxHeight: 420),
        decoration: BoxDecoration(
          // surfaceContainer 而不是 surface：弹层要比被它盖住的内容浅一档，
          // 深色下同用 surface 会让面板陷进背景里 —— 阴影在深底上几乎不可见，
          // 「浮起来」只能靠底色差
          color: scheme.surfaceContainer,
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
                    _revealSelected();
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
                      ? _status(theme)
                      : ListView.builder(
                          controller: _scroll,
                          shrinkWrap: true,
                          padding: const EdgeInsets.symmetric(
                            vertical: _listPad,
                          ),
                          // 行高固定，键盘跟随滚动才算得准（见 _rowExtent）
                          itemExtent: _rowExtent,
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
                                        mainAxisAlignment:
                                            MainAxisAlignment.center,
                                        crossAxisAlignment:
                                            CrossAxisAlignment.start,
                                        children: [
                                          Text(
                                            e.title,
                                            maxLines: 1,
                                            overflow: TextOverflow.ellipsis,
                                            style: theme.textTheme.bodyMedium
                                                ?.copyWith(
                                                  // 选中态补一档字重：底色
                                                  // 是中性的一档，扫视时
                                                  // 只靠它不够跳
                                                  fontWeight: selected
                                                      ? FontWeight.w600
                                                      : null,
                                                ),
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
                // 清单不完整这件事要说：不说的话，用户打了个名字搜不到，
                // 会以为那个文件不存在。放在列表外面而不是当最后一行 ——
                // itemExtent 要求每行同高，脚注混进去会破坏滚动数学
                if (mention != null && entries.isNotEmpty && mention.truncated)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                    child: Text(
                      '只收了前 $kMentionScanLimit 个',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: tokens.foregroundTertiary,
                      ),
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
