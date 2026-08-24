import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../core/formatting.dart';
import '../../core/motion.dart';
import '../../core/theme.dart';
import '../../models/chat_session.dart';
import '../../models/project.dart';
import '../../models/session_search_hit.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../state/chat_state.dart';
import '../../state/confirm_controller.dart';
import '../../core/session_export.dart';
import '../../state/export_controller.dart';
import '../../state/project_controller.dart';
import '../../state/sidebar_sections.dart';
import '../projects/project_actions.dart';
import '../../state/session_search_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';

/// Left pane: session switcher.
class SessionList extends ConsumerWidget {
  const SessionList({super.key, this.onSelected});

  /// Lets the narrow-layout drawer close itself after a pick.
  final VoidCallback? onSelected;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(chatControllerProvider);
    final controller = ref.read(chatControllerProvider.notifier);
    final projects = ref.watch(projectControllerProvider);
    final search = ref.watch(sessionSearchProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '会话',
          actions: [
            // 老服务端没有 /projects，那里这个按钮点下去只会得到一条
            // 用户无法处理的报错 —— 干脆不给
            if (!projects.unsupported)
              IconButton(
                onPressed: () => _createProject(context, ref),
                iconSize: 18,
                tooltip: '新建项目',
                icon: const Icon(Icons.create_new_folder_outlined),
              ),
            _ArchivedToggle(
              value: state.showArchived,
              onChanged: controller.setShowArchived,
            ),
            // 这里曾有第三个按钮「新对话」（add_rounded）。删掉了：左栏
            // NavBlock 首行已经是同一个动作的入口，同一件事两个入口还配着
            // 两个不同的图标，读起来像两个不同的功能 —— 留下的是首行那个
          ],
        ),
        // 待补写的那些。**必须画在这儿，不能只留在设置里。**
        //
        // 这个数字此前只出现在 设置 → 连接 那一页（两层菜单之外），
        // 而用户是在**这份列表**上发现「我昨天那段对话不见了」的 ——
        // 答案离问题四次点击远，等于没有答案。
        const _BacklogNote(),
        // 老服务端没有 /sessions/search —— 那里不画搜索框。
        // 画一个点下去只会得到 404 的框，比没有这个框更糟
        if (!search.unsupported) const _SearchBox(),
        Expanded(
          child: search.active
              ? _SearchResults(onSelected: onSelected)
              : _body(context, ref, state, projects, controller),
        ),
      ],
    );
  }

  Widget _body(
    BuildContext context,
    WidgetRef ref,
    ChatState state,
    ProjectState projects,
    ChatController controller,
  ) {
    if (state.sessionsLoading && state.sessions.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 18,
          height: 18,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    if (state.sessionsError != null && state.sessions.isEmpty) {
      return EmptyState(
        icon: Icons.cloud_off_rounded,
        title: '拉不到会话列表',
        description: state.sessionsError,
        tone: EmptyStateTone.error,
        action: OutlinedButton.icon(
          onPressed: controller.loadSessions,
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }

    final sessions = state.visibleSessions;
    // 有项目就得画出来，哪怕一条会话都没有 —— 刚建好的空项目正是要被拖进
    // 东西的那个空篮子，藏掉它等于新建按钮什么也没做
    if (sessions.isEmpty && !projects.showGrouping) {
      // ⚠️ **不给「新建」按钮。** 惰性建会话之后它会变成一个点了看不出
      // 反应的按钮：白纸不进左栏，而这一栏此刻就是空的。
      // 真正的入口是右边那个输入框 —— 说一句话就有了第一条对话
      return EmptyState(
        icon: Icons.forum_outlined,
        title: state.showArchived ? '还没有对话' : '没有未归档的对话',
        description: state.showArchived
            ? '在右边说一句话，第一条对话就出现在这里。'
            : '在右边说一句话就能开始。已归档的对话在右上角那个开关里。',
      );
    }

    final rows = _rows(sessions, projects, ref.watch(sidebarSectionsProvider));

    return ListView.builder(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 8),
      itemCount: rows.length,
      itemBuilder: (context, i) => switch (rows[i]) {
        _SectionRow(:final section, :final count) => _SectionHeader(
          key: ValueKey('section:${section.key}'),
          section: section,
          count: count,
          collapsed: ref
              .read(sidebarSectionsProvider.notifier)
              .isCollapsed(section),
          onToggle: () =>
              ref.read(sidebarSectionsProvider.notifier).toggle(section),
        ),
        _EmptySectionRow(:final section) => _EmptySectionHint(
          key: ValueKey('section_empty:${section.key}'),
          section: section,
        ),
        _HeaderRow(:final group) => _GroupHeader(
          key: ValueKey('grp_${group.projectId ?? "none"}'),
          group: group,
          collapsed:
              group.projectId != null && projects.isCollapsed(group.projectId!),
          selected: projects.selectedId == group.projectId,
          onToggle: () => group.projectId == null
              ? null
              : ref
                    .read(projectControllerProvider.notifier)
                    .toggleCollapsed(group.projectId!),
          onNewSession: () {
            final id = group.projectId;
            if (id != null) {
              ref.read(projectControllerProvider.notifier).expand(id);
            }
            // 白纸带着项目归属：真开口时那条会话才建出来，且落在这个项目下
            controller.startNewChat(projectId: id);
            ref.read(projectControllerProvider.notifier).select(id);
            onSelected?.call();
          },
          onRename: () => renameProject(context, ref, group.project!),
          onTogglePin: () => setProjectPinned(
            context,
            ref,
            group.project!,
            !group.project!.pinned,
          ),
          pinned: group.project?.pinned ?? false,
          onDelete: () => deleteProject(context, ref, group.project!),
        ),
        _SessionRow(:final session) => _SessionTile(
          key: ValueKey(session.id),
          session: session,
          selected: session.id == state.activeSessionId,
          // 「这个会话正在跑」有两个来源，**两个都要认**：
          //
          // 1. `state.streaming` —— 此刻这个客户端正连着的那一轮
          // 2. `state.unfinished` —— 发出去了、还没见到收尾的（用户切走了、
          //    甚至关掉过页面）。第 2 条正是「派出去干活」那个场景的落点，
          //    只认第 1 条的话，人一切走徽章就没了，而活还在干
          streaming:
              state.streaming?.sessionId == session.id ||
              state.unfinished.contains(session.id),
          // 第四状态：**在等你确认**。它必然发生在「在跑」当中（一轮跑到
          // 一半停下来问人），所以行上的优先级要压过 streaming —— 蓝点说
          // 「不用管」，而这个状态恰恰是唯一「不管就永远卡住」的
          awaitingConfirm: ref
              .watch(awaitingConfirmSessionsProvider)
              .contains(session.id),
          // 「跑完了，你还没看」。与「在跑」互斥 —— 同一条会话不可能
          // 既在跑又刚跑完（`_commit` 是先撤 unfinished 再加 finished）
          justFinished: state.finished.contains(session.id),
          canMove: projects.showGrouping,
          onTap: () {
            controller.selectSession(session.id);
            ref
                .read(projectControllerProvider.notifier)
                .select(session.projectId);
            onSelected?.call();
          },
          onRename: () => _rename(context, ref, session),
          onTogglePin: () => _pin(context, ref, session, !session.pinned),
          pinned: session.pinned,
          onToggleArchive: () =>
              _archive(context, ref, session, !session.archived),
          onMove: () => _move(context, ref, session, projects.projects),
          onExport: (format) => _export(context, ref, session, format),
        ),
      },
    );
  }

  /// 把三段摊平成一串行，交给 `ListView.builder` 按需构建。
  ///
  /// # ⚠️ 一条会话只出现在一段里
  ///
  /// 优先级是**置顶 > 属于置顶项目 > 其余**，而且是一条全序：
  ///
  /// 1. `pinned` 的会话 → 「Pinned」段
  /// 2. 否则，属于某个置顶项目的 → 「项目」段那棵树里
  /// 3. 否则 → 「聊天」段（未置顶的项目仍在这里以分组标题出现）
  ///
  /// 少了这条全序的下场是同一条会话在左栏有两行：点哪一行都对，
  /// 但用户会以为自己看重了，或者以为有两条一模一样的对话。
  ///
  /// 置顶优先于项目，是因为「我把这条钉起来了」是一个**更晚、更具体**的
  /// 意图 —— 项目归属可能是几周前顺手分的。
  ///
  /// # 三段**恒在**，空着也画
  ///
  /// 这里原先有两条「省掉噪音」的捷径：没有项目也没有置顶时整个退回一张
  /// 平铺列表，以及任何一段空着就不画它的段头。理由写的是「三个段头对着
  /// 一列会话，每一个都不传达信息，只是白占三行」。
  ///
  /// **那个理由算错了成本。** 空着的段头传达的恰恰是最重要的那件事：
  /// 这里可以放东西。置顶会话、置顶项目这两个功能在界面上**只有段头这一个
  /// 入口** —— 段头不画，用户就永远不知道能置顶，于是永远没有置顶，
  /// 于是段头永远不画。2026-08-23 用户报「左侧只有一个聊天的分组」，
  /// 正是走到了这个闭环里。
  ///
  /// 代价（三行常驻）是真的，但它是一次性的；发现不了功能是永久的。
  static List<_Row> _rows(
    List<ChatSession> sessions,
    ProjectState projects,
    Set<SidebarSection> collapsed,
  ) {
    final pinnedProjects = projects.projects.where((p) => p.pinned).toList();
    final pinnedSessions = sessions.where((s) => s.pinned).toList();

    final rows = <_Row>[];

    void section(SidebarSection which, int count, void Function() body) {
      rows.add(_SectionRow(which, count));
      if (collapsed.contains(which)) return;
      // 空段展开时给一行「这里能放什么」。**不是装饰** —— 它是置顶这个
      // 功能唯一的说明书：段头只说得出这一段叫什么，说不出怎么往里放东西
      if (count == 0) {
        rows.add(_EmptySectionRow(which));
        return;
      }
      body();
    }

    // 段序 2026-08-24 重排为 置顶 → 项目 → 最近（照设计稿）：
    // 「置顶」的定义就是用户要它在最上面；「最近」固定最下 —— 它是
    // 绝大多数进来的人要找的那一段。各段的过滤互相独立，重排不改归属。

    // ── 1. 置顶：置顶的会话，平铺 ──
    section(SidebarSection.pinned, pinnedSessions.length, () {
      rows.addAll(pinnedSessions.map(_SessionRow.new));
    });

    // ── 2. 项目：置顶的那些，每个可展开成它的会话 ──
    final pinnedIds = {for (final p in pinnedProjects) p.id};
    section(SidebarSection.projects, pinnedProjects.length, () {
      final byProject = groupSessionsByProject(
        // 置顶的会话已经被「置顶」段收走了，这里不能再算进来
        sessions.where((s) => !s.pinned).toList(),
        pinnedProjects,
      );
      for (final group in byProject) {
        if (group.isUngrouped) continue;
        rows.add(_HeaderRow(group));
        final id = group.projectId;
        if (id != null && projects.isCollapsed(id)) continue;
        rows.addAll(group.sessions.map(_SessionRow.new));
      }
    });

    // ── 3. 最近：其余的，**纯时间线，只有对话这一层** ──
    //
    // 这里曾按未置顶项目再分一层组（连带一个「未分组」组头）。删掉了
    // （2026-08-24 实测反馈：「最近里面应该只有对话这一层，怎么还有
    // 未分组」）：「最近」回答的是**什么时候**，项目回答的是**属于哪**，
    // 两个维度叠在同一段里，读者哪个都拿不稳 —— 时间线被组头切碎，
    // 分组又只覆盖没置顶的那半。项目维度有自己的两个入口
    // （置顶进「项目」段、全部在项目页），不缺这一处。
    // 参考 Claude 的左栏：Chats 就是一条平铺的时间线。
    final rest = sessions
        .where((s) => !s.pinned && !pinnedIds.contains(s.projectId))
        .toList();
    section(SidebarSection.chats, rest.length, () {
      rows.addAll(rest.map(_SessionRow.new));
    });

    return rows;
  }

  /// 导出这一段会话。
  ///
  /// 从服务端**整段**拉，与屏幕上加载了多少无关 —— 见 [ExportController]。
  static Future<void> _export(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
    ExportFormat format,
  ) async {
    final messenger = ScaffoldMessenger.of(context);
    messenger.showSnackBar(
      SnackBar(
        content: Text('正在导出「${session.title}」为 ${format.label}…'),
        duration: const Duration(seconds: 2),
      ),
    );
    await ref
        .read(exportControllerProvider.notifier)
        .export(session.id, format);
    final result = ref.read(exportControllerProvider);
    messenger.hideCurrentSnackBar();
    if (result.error != null) {
      messenger.showSnackBar(SnackBar(content: Text('导出失败：${result.error}')));
      return;
    }
    // 用户自己取消了「另存为」—— 不该弹任何提示
    final name = result.savedName;
    if (name == null) return;
    messenger.showSnackBar(
      SnackBar(
        content: Text(
          result.truncated
              // 不完整的存档必须当场说。悄悄少一半比导不出来更糟 ——
              // 后者用户会重试，前者他三个月后才会发现
              ? '已导出 $name，但这段会话太长，只导出了最近的 ${result.episodeCount} 条'
              : '已导出 $name（${result.episodeCount} 条消息）',
        ),
      ),
    );
  }

  // --------------------------------------------------------------- projects

  Future<void> _createProject(BuildContext context, WidgetRef ref) =>
      createProject(context, ref);

  /// 「移动到项目」。列出全部项目加一个「未分组」，当前那一项打勾。
  ///
  /// 用弹出列表而不是二级菜单：候选项是用户自己建的，可能有十几个，
  /// 悬停展开的二级菜单在触摸屏上根本没法用。
  Future<void> _move(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
    List<Project> projects,
  ) async {
    final picked = await showDialog<_MoveChoice>(
      context: context,
      builder: (context) => SimpleDialog(
        title: const Text('移动到项目'),
        children: [
          _MoveOption(
            label: '未分组',
            icon: Icons.forum_outlined,
            current: session.projectId == null,
            onTap: () => Navigator.of(context).pop(const _MoveChoice(null)),
          ),
          for (final p in projects)
            _MoveOption(
              label: p.name,
              icon: Icons.folder_outlined,
              current: session.projectId == p.id,
              onTap: () => Navigator.of(context).pop(_MoveChoice(p.id)),
            ),
        ],
      ),
    );
    if (picked == null || picked.projectId == session.projectId) return;
    if (!context.mounted) return;
    await _run(
      context,
      () => ref
          .read(chatControllerProvider.notifier)
          .moveSessionToProject(session.id, picked.projectId),
    );
  }

  // --------------------------------------------------------------- sessions

  /// A derived title is a placeholder, not a name, so the field starts empty
  /// when the user has never set one — otherwise pressing OK without typing
  /// would quietly promote the placeholder to a real title, and the session
  /// would stop tracking its first message.
  Future<void> _rename(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
  ) async {
    final controller = TextEditingController(
      text: session.titleIsCustom ? session.title : '',
    );
    final result = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('重命名会话'),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: InputDecoration(
            labelText: '标题',
            hintText: session.titleIsCustom ? null : session.title,
            helperText: session.titleIsCustom ? null : '留空则继续沿用首条消息派生的标题',
          ),
          onSubmitted: (v) => Navigator.of(context).pop(v),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(controller.text),
            child: const Text('保存'),
          ),
        ],
      ),
    );
    controller.dispose();
    if (result == null || result.trim().isEmpty) return;
    if (!context.mounted) return;
    await _run(
      context,
      () => ref
          .read(chatControllerProvider.notifier)
          .renameSession(session.id, result),
    );
  }

  Future<void> _archive(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
    bool archived,
  ) async {
    await _run(
      context,
      () => ref
          .read(chatControllerProvider.notifier)
          .setArchived(session.id, archived),
    );
  }

  /// Surfaces a failed mutation instead of letting it disappear into a future
  /// nobody awaits. An *unsupported* endpoint never reaches here for sessions —
  /// the controller absorbs that case and marks the session as locally edited.
  /// For projects it *does* reach here, deliberately: there is no meaningful
  /// local-only project, so the honest outcome is to say it did not happen.
  /// 置顶 / 取消置顶一条会话。
  ///
  /// 控制器那边会**核对服务端真的改了** —— 老服务端静默忽略 `pinned` 并回
  /// 200，不核对的话这一行当场搬进 Pinned 段、刷新又弹回来。它回一句话就
  /// 在这里原样说出去。
  static Future<void> _pin(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
    bool pinned,
  ) async {
    final messenger = ScaffoldMessenger.maybeOf(context);
    final said = await ref
        .read(chatControllerProvider.notifier)
        .setPinned(session.id, pinned);
    if (said != null) {
      messenger?.showSnackBar(SnackBar(content: Text(said)));
    }
  }

  static Future<void> _run(
    BuildContext context,
    Future<void> Function() action,
  ) async {
    final messenger = ScaffoldMessenger.maybeOf(context);
    try {
      await action();
    } on CortexApiException catch (e) {
      messenger?.showSnackBar(SnackBar(content: Text(e.message)));
    } on Object catch (e) {
      messenger?.showSnackBar(SnackBar(content: Text('$e')));
    }
  }
}

/// 「移到哪里」的返回值。
///
/// 包一层是因为 `null` 在这里有两个意思：对话框被取消，和「移到未分组」。
/// 直接返回 `String?` 的话这两件完全相反的事会长得一模一样。
class _MoveChoice {
  const _MoveChoice(this.projectId);

  final String? projectId;
}

class _MoveOption extends StatelessWidget {
  const _MoveOption({
    required this.label,
    required this.icon,
    required this.current,
    required this.onTap,
  });

  final String label;
  final IconData icon;
  final bool current;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SimpleDialogOption(
      onPressed: onTap,
      child: Row(
        children: [
          Icon(icon, size: 16, color: scheme.onSurfaceVariant),
          const SizedBox(width: 10),
          Expanded(child: Text(label, overflow: TextOverflow.ellipsis)),
          if (current)
            Icon(Icons.check_rounded, size: 16, color: scheme.primary),
        ],
      ),
    );
  }
}

sealed class _Row {
  const _Row();
}

/// 三段之一的段头。
class _SectionRow extends _Row {
  const _SectionRow(this.section, this.count);

  final SidebarSection section;

  /// 这一段里有几条。**是「现在看得见几条」**，不是服务端那个统计 ——
  /// 旁边就是那些行，写一个对不上的数字只会让人以为界面漏了东西
  final int count;
}

/// 空着的那一段下面那行说明。
///
/// 只在展开且 `count == 0` 时出现。折起来时不画 —— 折叠的意思是
/// 「这一段我现在不看」，那时连提示都是打扰。
class _EmptySectionRow extends _Row {
  const _EmptySectionRow(this.section);

  final SidebarSection section;
}

class _HeaderRow extends _Row {
  const _HeaderRow(this.group);

  final SessionGroup group;
}

class _SessionRow extends _Row {
  const _SessionRow(this.session);

  final ChatSession session;
}

/// 三段的段头：标题 + 折叠箭头 + 条数。
///
/// 与项目那一行（[_GroupHeader]）**故意长得不一样**：它是更高一层的东西，
/// 长一样的话用户分不出「项目」这个段头和一个叫「项目」的项目。
class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    super.key,
    required this.section,
    required this.count,
    required this.collapsed,
    required this.onToggle,
  });

  final SidebarSection section;
  final int count;
  final bool collapsed;
  final VoidCallback onToggle;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;

    return Padding(
      // 上面留得多：段头属于**它下面那一段**，不是两段之间的分隔物
      padding: const EdgeInsets.only(top: 16, bottom: 2),
      child: InkWell(
        onTap: onToggle,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(4, 4, 6, 4),
          child: Row(
            children: [
              Text(
                section.label,
                style: theme.textTheme.labelSmall?.copyWith(
                  fontWeight: FontWeight.w700,
                  letterSpacing: 0.8,
                  color: tokens.foregroundTertiary,
                ),
              ),
              const SizedBox(width: 6),
              Text(
                '$count',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: tokens.foregroundTertiary,
                ),
              ),
              const Spacer(),
              Icon(
                collapsed
                    ? Icons.chevron_right_rounded
                    : Icons.expand_more_rounded,
                size: 15,
                color: tokens.foregroundTertiary,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 空着的那一段下面那一行字。
///
/// 刻意**不是**一个带边框的空状态卡片：它挨着段头、缩进对齐会话行，
/// 读起来像「这一段的内容」而不是「一个错误提示」。左栏那么窄，
/// 任何带框的东西都会把三段撑成三个盒子。
class _EmptySectionHint extends StatelessWidget {
  const _EmptySectionHint({super.key, required this.section});

  final SidebarSection section;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      // 左边与会话行的文字对齐，不是与段头对齐 —— 它说的是「这一段里」
      padding: const EdgeInsets.fromLTRB(10, 2, 10, 6),
      child: Text(
        section.emptyHint,
        style: theme.textTheme.labelSmall?.copyWith(
          color: theme.cortex.foregroundTertiary,
          height: 1.4,
        ),
      ),
    );
  }
}

class _GroupHeader extends StatelessWidget {
  const _GroupHeader({
    super.key,
    required this.group,
    required this.collapsed,
    required this.selected,
    required this.onToggle,
    required this.onNewSession,
    required this.onRename,
    required this.onTogglePin,
    required this.pinned,
    required this.onDelete,
  });

  final SessionGroup group;
  final bool collapsed;
  final bool selected;
  final VoidCallback onToggle;
  final VoidCallback onNewSession;
  final VoidCallback onRename;
  final VoidCallback onTogglePin;
  final bool pinned;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final project = group.project;

    return Padding(
      // 上面 14 下面 2：分组头属于**它下面那一组**，不是上下均分的
      // 分隔物。均分的话它会读成「两组之间的一条线」，而不是
      // 「这一组的标题」—— 此前 6/2 太挤，一列扫下来看不出分组
      padding: const EdgeInsets.only(top: 14, bottom: 2),
      child: InkWell(
        onTap: project == null ? null : onToggle,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(4, 3, 2, 3),
          child: Row(
            children: [
              Icon(
                project == null
                    ? Icons.forum_outlined
                    : collapsed
                    ? Icons.chevron_right_rounded
                    : Icons.expand_more_rounded,
                size: 16,
                color: scheme.onSurfaceVariant,
              ),
              const SizedBox(width: 4),
              Flexible(
                child: Text(
                  project?.name ?? '未分组',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall?.copyWith(
                    fontWeight: FontWeight.w600,
                    letterSpacing: 0.6,
                    color: selected && project != null
                        ? scheme.primary
                        : scheme.onSurfaceVariant,
                  ),
                ),
              ),
              const SizedBox(width: 6),
              // 这里显示的是**现在看得见几条**，与 `Project.sessionCount`
              // （含已归档）故意不同：这一行旁边就是那些会话本身，
              // 写一个和眼前对不上的数字只会让人以为界面漏了东西
              Text(
                '${group.sessions.length}',
                style: theme.textTheme.labelSmall,
              ),
              const Spacer(),
              if (project != null) ...[
                IconButton(
                  onPressed: onNewSession,
                  iconSize: 15,
                  padding: EdgeInsets.zero,
                  constraints: const BoxConstraints.tightFor(
                    width: 26,
                    height: 26,
                  ),
                  tooltip: '在「${project.name}」里新建会话',
                  icon: const Icon(Icons.add_rounded),
                ),
                _ProjectMenu(
                  projectName: project.name,
                  pinned: pinned,
                  onRename: onRename,
                  onTogglePin: onTogglePin,
                  onDelete: onDelete,
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _ProjectMenu extends StatelessWidget {
  const _ProjectMenu({
    required this.projectName,
    required this.pinned,
    required this.onRename,
    required this.onTogglePin,
    required this.onDelete,
  });

  /// 只用来构造那句 tooltip。会话行的菜单是空 tooltip（一列同样的图标，
  /// 每一个都弹一句话很吵），项目这一行只有一两个，说清是**哪个**项目
  /// 反而有用 —— 顺带让测试有一个稳定的抓手。
  final String projectName;
  final bool pinned;
  final VoidCallback onRename;
  final VoidCallback onTogglePin;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<String>(
      tooltip: '「$projectName」的更多操作',
      iconSize: 15,
      padding: EdgeInsets.zero,
      splashRadius: 14,
      icon: const Icon(Icons.more_horiz_rounded),
      onSelected: (value) => switch (value) {
        'rename' => onRename(),
        'pin' => onTogglePin(),
        _ => onDelete(),
      },
      itemBuilder: (context) => [
        PopupMenuItem(
          value: 'pin',
          height: 38,
          child: Row(
            children: [
              Icon(
                pinned ? Icons.push_pin_outlined : Icons.push_pin_rounded,
                size: 15,
              ),
              const SizedBox(width: 9),
              // 取消置顶之后它不会消失，只是从左栏挪回项目页 ——
              // 文案要让人敢按
              Text(pinned ? '取消置顶' : '置顶到左栏'),
            ],
          ),
        ),
        const PopupMenuItem(
          value: 'rename',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.edit_outlined, size: 15),
              SizedBox(width: 9),
              Text('重命名项目'),
            ],
          ),
        ),
        const PopupMenuItem(
          value: 'delete',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.folder_delete_outlined, size: 15),
              SizedBox(width: 9),
              // 敢叫「删除」是因为删的真的只是这层分组 —— 会话一条不少。
              // 会话那边永远只叫「归档」，因为那里删了就没了
              Text('删除项目'),
            ],
          ),
        ),
      ],
    );
  }
}

class _ArchivedToggle extends StatelessWidget {
  const _ArchivedToggle({required this.value, required this.onChanged});

  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return IconButton(
      onPressed: () => onChanged(!value),
      iconSize: 17,
      tooltip: value ? '隐藏已归档' : '显示已归档',
      isSelected: value,
      color: value ? scheme.primary : null,
      icon: Icon(
        value ? Icons.inventory_2_rounded : Icons.inventory_2_outlined,
      ),
    );
  }
}

class _SessionTile extends StatefulWidget {
  const _SessionTile({
    super.key,
    required this.session,
    required this.selected,
    required this.streaming,
    required this.awaitingConfirm,
    required this.justFinished,
    required this.canMove,
    required this.onTap,
    required this.onRename,
    required this.onTogglePin,
    required this.pinned,
    required this.onToggleArchive,
    required this.onMove,
    required this.onExport,
  });

  final ChatSession session;
  final bool selected;
  final bool streaming;

  /// 第四状态：一轮跑到一半，**在等这个人点确认**。
  /// 行上压过 [streaming] —— 蓝点的意思是「不用管」，而这个状态不管就永远卡住。
  final bool awaitingConfirm;

  /// 人不在的时候跑完了，还没被打开过。
  final bool justFinished;

  /// 这个部署有项目功能吗。没有的话「移动到项目」不进菜单 —— 一个点下去
  /// 只会报错的菜单项，比没有这个菜单项更糟。
  final bool canMove;
  final VoidCallback onTap;
  final VoidCallback onRename;
  final VoidCallback onTogglePin;

  /// 置顶了 —— 这一行此刻住在「Pinned」那一段里。
  final bool pinned;
  final VoidCallback onToggleArchive;
  final VoidCallback onMove;
  final void Function(ExportFormat) onExport;

  @override
  State<_SessionTile> createState() => _SessionTileState();
}

class _SessionTileState extends State<_SessionTile> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final session = widget.session;

    final tokens = theme.cortex;
    // 「当前在看哪个会话」是**位置**，不是动作 —— 所以用中性的一档，
    // 不用品牌色。此前是 `primary.withValues(alpha: .12)`，于是整条侧栏
    // 常年挂着一块紫，把真正需要注意的东西一起稀释掉了。见 docs/design.md
    final Color background;
    if (widget.selected) {
      background = tokens.sidebarAccent;
    } else if (_hovered) {
      // 悬停比选中再浅一档：两者同色的话，鼠标扫过时每一行看起来都像
      // 被选中了，而真正选中的那一行反而认不出来
      background = tokens.sidebarAccent.withValues(alpha: 0.55);
    } else {
      background = Colors.transparent;
    }

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          // 悬停/选中的底色渐变。**这一处此前没读「减少动效」** ——
          // 那个开关只接了三个循环指示器，于是关掉之后鼠标扫过侧栏
          // 每一行仍然在渐变，而侧栏正是鼠标经过最频繁的地方
          duration: motionDuration(context, const Duration(milliseconds: 110)),
          // 左右各留一点：选中块贴着侧栏两边时，它读成「整条栏换了颜色」
          // 而不是「这一项被选中了」。上下的 3 给相邻两项一点呼吸 ——
          // 此前是 2，一列下来是一堵字墙
          margin: const EdgeInsets.fromLTRB(6, 1, 6, 2),
          padding: const EdgeInsets.fromLTRB(10, 8, 4, 8),
          decoration: BoxDecoration(
            color: background,
            borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
          ),
          // 已归档 = **整行**压暗（对齐「关掉的技能整行压暗」的做法）：
          // 压的是这一行整体 —— 时间戳、徽标、状态点一起退后，而不是给
          // 标题文字单独调透明度。后者把层级表达混进前景色里，与上面那条
          // 「前景全实色」互相打架
          child: Opacity(
            opacity: session.archived ? 0.45 : 1,
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(
                        children: [
                          // 这里曾有一个「绑定了工作区」的 teal 文件夹小图标。
                          // 删掉了：对用户它是一个看不懂的绿色徽章（2026-08-24
                          // 实测反馈原话「绿色图标又是什么」）—— 绑没绑目录是
                          // 会话**内部**的事，输入框的工作区 chip 与右栏都在
                          // 说它；左栏一行的职责只有「这是哪条对话、什么状态」。
                          // 实现细节漏成用户可见的元素，是这个仓库记过的形状
                          Expanded(
                            child: Text(
                              session.title,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              // 前景一律实色，选中与否只差字重 —— 层级不靠
                              // 「更亮一点」表达。透明度分级的下场是同一段字
                              // 在不同表面上深浅不一，还与禁用态的半透明撞车
                              //（见 CortexTokens.foregroundTertiary 的注）
                              style: theme.textTheme.bodyMedium?.copyWith(
                                fontWeight: widget.selected
                                    ? FontWeight.w600
                                    : FontWeight.w400,
                                color: scheme.onSurface,
                              ),
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 2),
                      Row(
                        children: [
                          Text(
                            formatRelative(session.updatedAt),
                            style: theme.textTheme.labelSmall,
                          ),
                          if (session.archived) ...[
                            const SizedBox(width: 6),
                            Text('已归档', style: theme.textTheme.labelSmall),
                          ],
                          if (session.isLocalDraft) ...[
                            const SizedBox(width: 6),
                            Text(
                              '未同步',
                              style: theme.textTheme.labelSmall?.copyWith(
                                color: scheme.onSurfaceVariant.withValues(
                                  alpha: 0.7,
                                ),
                              ),
                            ),
                          ],
                          if (session.hasLocalOverrides) ...[
                            const SizedBox(width: 6),
                            Tooltip(
                              // Said out loud because the alternative is a
                              // rename that looks saved and silently is not.
                              message:
                                  'cortexd 还没有 PATCH /sessions/{id}，'
                                  '这次改动只存在于本地，重启后会消失。',
                              child: Text(
                                '仅本地',
                                style: theme.textTheme.labelSmall?.copyWith(
                                  // 琥珀 —— 它是「重启会丢」的警告。⚠️ 不用
                                  // scheme.tertiary：这套主题没定义 tertiary，
                                  // M3 会静默回落成 teal，把警告画成安心绿
                                  color: tokens.warning,
                                ),
                              ),
                            ),
                          ],
                        ],
                      ),
                    ],
                  ),
                ),
                // 琥珀点压过蓝点：等确认必然发生在「在跑」当中，两个都真时
                // 该喊的是「等你」。外加一圈同色描边把它与蓝点在形状上也
                // 区分开 —— 色弱视角下「实心点 vs 带环的点」仍然分得出
                if (widget.awaitingConfirm)
                  Tooltip(
                    message: '在等你确认才能继续',
                    child: Container(
                      width: 8,
                      height: 8,
                      margin: const EdgeInsets.only(right: 5),
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        border: Border.all(color: tokens.warning, width: 2),
                      ),
                    ),
                  )
                else if (widget.streaming)
                  // 三个状态标记里它此前唯一没有 Tooltip。点本身不识字 ——
                  // 悬停说得出口，人才学得会认它
                  Tooltip(
                    message: '还在跑，不用等',
                    child: Container(
                      width: 6,
                      height: 6,
                      margin: const EdgeInsets.only(right: 6),
                      decoration: BoxDecoration(
                        color: scheme.primary,
                        shape: BoxShape.circle,
                      ),
                    ),
                  )
                // 用勾而不是第二个圆点：两个只有颜色不同的点，在色弱视角下
                // 与「在跑」分不开，而这两件事的下一步完全相反
                else if (widget.justFinished)
                  Padding(
                    padding: const EdgeInsets.only(right: 5),
                    child: Tooltip(
                      message: '这一轮跑完了，你还没看',
                      child: Icon(
                        Icons.check_circle_rounded,
                        size: 13,
                        // 绿 = 完成。此前与 streaming 蓝点同用 primary ——
                        // 两个下一步完全相反的状态共享同一个色彩通道
                        color: tokens.success,
                      ),
                    ),
                  ),
                // Kept mounted rather than built on hover: a menu that appears
                // under the cursor shifts the row's layout the moment the pointer
                // arrives, which makes the title jitter as the mouse crosses it.
                Opacity(
                  opacity: _hovered || widget.selected ? 1 : 0,
                  child: SessionTileMenu(
                    archived: session.archived,
                    enabled: _hovered || widget.selected,
                    canMove: widget.canMove,
                    onRename: widget.onRename,
                    onTogglePin: widget.onTogglePin,
                    pinned: widget.pinned,
                    onToggleArchive: widget.onToggleArchive,
                    onMove: widget.onMove,
                    onExport: widget.onExport,
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

/// 会话那一行右边的「…」。
///
/// # 离线时前三项是灰的
///
/// 标题、项目归属、归档状态的权威**都在服务器上**：`cortex-local` 的
/// `patch_session` 只把 `workspace` 留在本机，其余原样转发。离线时那条
/// 转发回 502，`_patch` 因为不是 404/405/501 而重新抛出，最后落成一个
/// SnackBar。
///
/// 也就是说它**不会丢数据**—— 但它是**事后**才说的：用户已经打完字、
/// 点完确定，才被告知这件事做不了。灰掉是事前说，而「导出」留着能点，
/// 因为那件事确实只发生在这台机器上。
///
/// # 为什么是公开类型
///
/// **只为了测得到。** 它在真实布局里被一道悬停门挡着
/// （`_SessionTile` 只在 `_hovered || selected` 时才让它 `enabled`，
/// 平时 opacity 为 0），widget 测试里既点不开也不稳定 —— 而它的全部内容
/// 都只在弹出之后才存在。公开之后测试直接挂它，跳过那道门。
class SessionTileMenu extends ConsumerWidget {
  const SessionTileMenu({
    super.key,
    required this.archived,
    required this.enabled,
    required this.canMove,
    required this.onRename,
    required this.onTogglePin,
    required this.pinned,
    required this.onToggleArchive,
    required this.onMove,
    required this.onExport,
  });

  final bool archived;
  final bool enabled;
  final bool canMove;
  final VoidCallback onRename;
  final VoidCallback onTogglePin;

  /// 置顶了 —— 菜单里那一项因此说「取消置顶」。
  final bool pinned;
  final VoidCallback onToggleArchive;
  final VoidCallback onMove;
  final void Function(ExportFormat) onExport;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final offline = ref.watch(appConfigProvider).offline;
    return PopupMenuButton<String>(
      enabled: enabled,
      tooltip: '',
      iconSize: 15,
      padding: EdgeInsets.zero,
      splashRadius: 14,
      icon: const Icon(Icons.more_horiz_rounded),
      onSelected: (value) => switch (value) {
        'rename' => onRename(),
        'pin' => onTogglePin(),
        'move' => onMove(),
        'export_md' => onExport(ExportFormat.markdown),
        'export_json' => onExport(ExportFormat.json),
        _ => onToggleArchive(),
      },
      itemBuilder: (context) => [
        PopupMenuItem(
          value: 'rename',
          height: 38,
          enabled: !offline,
          child: const Row(
            children: [
              Icon(Icons.edit_outlined, size: 15),
              SizedBox(width: 9),
              Text('重命名'),
            ],
          ),
        ),
        PopupMenuItem(
          value: 'pin',
          height: 38,
          enabled: !offline,
          child: Row(
            children: [
              Icon(
                pinned ? Icons.push_pin_outlined : Icons.push_pin_rounded,
                size: 15,
              ),
              const SizedBox(width: 9),
              // 置顶**不改变可见性**，只是搬进左栏那一段 ——
              // 与归档（从默认列表消失）是两件完全不同的事
              Text(pinned ? '取消置顶' : '置顶'),
            ],
          ),
        ),
        if (canMove)
          PopupMenuItem(
            value: 'move',
            height: 38,
            enabled: !offline,
            child: const Row(
              children: [
                Icon(Icons.drive_file_move_outlined, size: 15),
                SizedBox(width: 9),
                Text('移动到项目'),
              ],
            ),
          ),
        // 两个格式各占一项，而不是一个「导出…」再弹一个对话框：
        // 用户点进来时已经知道自己要哪个，多一次点击只是拦路
        const PopupMenuItem(
          value: 'export_md',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.description_outlined, size: 15),
              SizedBox(width: 9),
              Text('导出 Markdown'),
            ],
          ),
        ),
        const PopupMenuItem(
          value: 'export_json',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.data_object_rounded, size: 15),
              SizedBox(width: 9),
              Text('导出 JSON'),
            ],
          ),
        ),
        PopupMenuItem(
          value: 'archive',
          height: 38,
          enabled: !offline,
          child: Row(
            children: [
              Icon(
                archived ? Icons.unarchive_outlined : Icons.archive_outlined,
                size: 15,
              ),
              const SizedBox(width: 9),
              // Never "删除": the store is append-only and nothing is
              // destroyed, so promising deletion here would be a lie the user
              // would only discover by looking for the data later.
              Text(archived ? '取消归档' : '归档'),
            ],
          ),
        ),
        // 一句话解释那三项为什么是灰的，而不是给每一项各加一个后缀 ——
        // 三份「（离线时改不了）」是同一句话说三遍，而灰掉的原因只有一个
        if (offline)
          PopupMenuItem(
            enabled: false,
            height: 30,
            child: Text(
              '离线时改不了 —— 标题、项目、归档都在服务器上',
              style: Theme.of(context).textTheme.labelSmall,
            ),
          ),
      ],
    );
  }
}

// ─────────────────────────── 搜索 ───────────────────────────

/// 侧栏顶部那个搜索框。
///
/// 自己拿着 `TextEditingController` 而不是每帧从 provider 读文本：
/// 后者会在每次异步状态变化时重建输入框，而重建时写 `controller.text`
/// 会把光标弹回行尾 —— 中文输入法下那意味着**打不完一个词**。
/// 「还有 N 条等着补写」。
///
/// # 为什么这一条值得占一行像素
///
/// 离线聊过的那几轮排在 `cortex-local` 的 outbox 里，联网后由后台循环
/// 按序灌回服务端。整个过程是**收敛**的，但在灌完之前，那些会话
/// 本地有、服务端没有 —— 而会话列表读的是服务端。
///
/// 也就是说：重连之后有一段时间，用户在这份列表上看不到自己昨天聊的东西。
/// 那不是数据丢了，但**没有任何东西告诉他这一点**，看起来就是丢了。
///
/// 数字为 0（绝大多数时候）什么都不画：一个常驻的「0 条待同步」是噪音，
/// 而噪音会让真的有积压的那一次也被忽略。
class _BacklogNote extends ConsumerWidget {
  const _BacklogNote();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final backlog = ref.watch(healthProvider).value?.server?.backlog ?? 0;
    if (backlog <= 0) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(8, 0, 8, 6),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
      decoration: BoxDecoration(
        color: scheme.secondaryContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.cloud_sync_outlined,
            size: 15,
            color: scheme.onSecondaryContainer,
          ),
          const SizedBox(width: 8),
          Expanded(
            // 说清**它会自己好**。不说的话，一个「还有 3 条没同步」的提示
            // 只会让人去找一个并不存在的「立即同步」按钮
            child: Text(
              '还有 $backlog 条离线时的对话在补写，完成后会出现在这份列表里。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSecondaryContainer,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _SearchBox extends ConsumerStatefulWidget {
  const _SearchBox();

  @override
  ConsumerState<_SearchBox> createState() => _SearchBoxState();
}

class _SearchBoxState extends ConsumerState<_SearchBox> {
  final _controller = TextEditingController();
  final _focus = FocusNode();

  @override
  void dispose() {
    _controller.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _clear() {
    _controller.clear();
    ref.read(sessionSearchProvider.notifier).clear();
  }

  @override
  Widget build(BuildContext context) {
    final search = ref.watch(sessionSearchProvider);
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      child: CallbackShortcuts(
        // Esc 退出搜索 —— 焦点在搜索框里时，那是所有人都会先按的那个键
        bindings: {
          const SingleActivator(LogicalKeyboardKey.escape): () {
            if (_controller.text.isEmpty) {
              _focus.unfocus();
            } else {
              _clear();
            }
          },
        },
        child: TextField(
          controller: _controller,
          focusNode: _focus,
          textInputAction: TextInputAction.search,
          onChanged: ref.read(sessionSearchProvider.notifier).setQuery,
          decoration: InputDecoration(
            isDense: true,
            hintText: '搜索会话与消息',
            prefixIcon: const Icon(Icons.search_rounded, size: 18),
            prefixIconConstraints: const BoxConstraints(minWidth: 34),
            suffixIcon: search.active
                ? IconButton(
                    onPressed: _clear,
                    iconSize: 16,
                    tooltip: '清空',
                    icon: const Icon(Icons.close_rounded),
                  )
                : null,
            border: const OutlineInputBorder(),
            contentPadding: const EdgeInsets.symmetric(vertical: 10),
          ),
        ),
      ),
    );
  }
}

class _SearchResults extends ConsumerWidget {
  const _SearchResults({this.onSelected});

  final VoidCallback? onSelected;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final search = ref.watch(sessionSearchProvider);
    final state = ref.watch(chatControllerProvider);

    if (search.error != null) {
      return EmptyState(
        icon: Icons.cloud_off_rounded,
        title: '搜索失败',
        description: search.error,
        tone: EmptyStateTone.error,
        action: OutlinedButton.icon(
          onPressed: ref.read(sessionSearchProvider.notifier).retry,
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }

    // 有旧结果时不换成转圈：连着打字的每一次防抖到期都会闪一下空白，
    // 而用户正在看的那份结果与新的通常只差一两条
    if (search.loading && search.hits.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 18,
          height: 18,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    if (search.hits.isEmpty) {
      return const EmptyState(
        icon: Icons.search_off_rounded,
        title: '没有匹配的会话',
        description: '搜的是标题与消息正文。已归档的会话不在结果里。',
      );
    }

    final needle = search.query.trim();
    return ListView.builder(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 8),
      itemCount: search.hits.length,
      itemBuilder: (context, i) {
        final hit = search.hits[i];
        return _SearchTile(
          key: ValueKey('hit_${hit.sessionId}'),
          hit: hit,
          needle: needle,
          // 标题为 null = 从没改过名。回落到侧栏列表里那个派生标题，
          // 而不是显示裸 id —— 同一个会话在两处该是同一个名字。
          // 列表里没有（归档的、超出那 200 条的）才退到占位文案
          fallbackTitle: _titleOf(state, hit.sessionId),
          selected: hit.sessionId == state.activeSessionId,
          onTap: () {
            ref
                .read(chatControllerProvider.notifier)
                .selectSession(hit.sessionId);
            onSelected?.call();
          },
        );
      },
    );
  }

  static String _titleOf(ChatState state, String sessionId) {
    for (final s in state.sessions) {
      if (s.id == sessionId) return s.title;
    }
    return '未命名会话';
  }
}

class _SearchTile extends StatelessWidget {
  const _SearchTile({
    super.key,
    required this.hit,
    required this.needle,
    required this.fallbackTitle,
    required this.selected,
    required this.onTap,
  });

  final SessionSearchHit hit;
  final String needle;
  final String fallbackTitle;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final title = hit.title ?? fallbackTitle;
    // 标题命中时不显示摘录：命中的地方已经在标题里摆着了，再补一段正文
    // 只会把别的结果挤出屏幕
    final excerpt = hit.titleMatch ? null : hit.excerpt;

    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Material(
        // **中性**（规范第九节）。搜索结果与上面那份会话列表画在**同一栏**
        // 里，而那边早就换成中性了 —— 这一处留着品牌色，表现是搜一下、
        // 选中项忽然变成一块紫，与刚才同一位置的选中态长得完全不同
        color: selected ? theme.cortex.sidebarAccent : Colors.transparent,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        child: InkWell(
          onTap: onTap,
          borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Expanded(
                      child: _highlight(
                        title,
                        needle,
                        theme.textTheme.bodyMedium,
                        scheme,
                        maxLines: 1,
                      ),
                    ),
                    if (hit.hitCount > 0) ...[
                      const SizedBox(width: 6),
                      Text(
                        '${hit.hitCount} 条',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ],
                ),
                if (excerpt != null && excerpt.isNotEmpty) ...[
                  const SizedBox(height: 2),
                  _highlight(
                    excerpt,
                    needle,
                    theme.textTheme.bodySmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                    scheme,
                    maxLines: 2,
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// 把命中的那几个字标出来。
  ///
  /// 不标的话，一段 160 字的摘录里用户还得自己找那个词在哪 —— 而他刚刚
  /// 才把它打进搜索框。大小写不敏感，与服务端那侧的 `ILIKE` 一致。
  static Widget _highlight(
    String text,
    String needle,
    TextStyle? style,
    ColorScheme scheme, {
    required int maxLines,
  }) {
    final spans = <TextSpan>[];
    final lowerText = text.toLowerCase();
    final lowerNeedle = needle.toLowerCase();
    var at = 0;
    // 空词走不到这里（那时搜索没激活），但真进来了也要能收场：
    // `indexOf` 找空串恒为 0，不判空就是死循环
    if (lowerNeedle.isNotEmpty) {
      while (true) {
        final found = lowerText.indexOf(lowerNeedle, at);
        if (found < 0) break;
        if (found > at) spans.add(TextSpan(text: text.substring(at, found)));
        spans.add(
          TextSpan(
            text: text.substring(found, found + needle.length),
            style: TextStyle(
              color: scheme.primary,
              fontWeight: FontWeight.w600,
            ),
          ),
        );
        at = found + needle.length;
      }
    }
    if (at < text.length) spans.add(TextSpan(text: text.substring(at)));

    return Text.rich(
      TextSpan(style: style, children: spans),
      maxLines: maxLines,
      overflow: TextOverflow.ellipsis,
    );
  }
}
