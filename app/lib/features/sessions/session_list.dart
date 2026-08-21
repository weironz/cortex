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
import '../../core/session_export.dart';
import '../../state/export_controller.dart';
import '../../state/project_controller.dart';
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
            IconButton(
              onPressed: () {
                controller.createSession();
                ref.read(projectControllerProvider.notifier).select(null);
                onSelected?.call();
              },
              iconSize: 18,
              tooltip: '新建会话',
              icon: const Icon(Icons.add_rounded),
            ),
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
      return EmptyState(
        icon: Icons.forum_outlined,
        title: state.showArchived ? '还没有会话' : '没有未归档的会话',
        description: state.showArchived ? null : '打开右上角的归档开关可以看到已归档的会话。',
        action: FilledButton.icon(
          onPressed: () {
            controller.createSession();
            onSelected?.call();
          },
          icon: const Icon(Icons.add_rounded, size: 18),
          label: const Text('新建会话'),
        ),
      );
    }

    final rows = _rows(sessions, projects);

    return ListView.builder(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 8),
      itemCount: rows.length,
      itemBuilder: (context, i) => switch (rows[i]) {
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
            controller.createSession(projectId: id);
            ref.read(projectControllerProvider.notifier).select(id);
            onSelected?.call();
          },
          onRename: () => _renameProject(context, ref, group.project!),
          onDelete: () => _deleteProject(context, ref, group.project!),
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
          onToggleArchive: () =>
              _archive(context, ref, session, !session.archived),
          onMove: () => _move(context, ref, session, projects.projects),
          onExport: (format) => _export(context, ref, session, format),
        ),
      },
    );
  }

  /// 把分组摊平成一串行，交给 `ListView.builder` 按需构建。
  ///
  /// 没有项目时**不画任何标题**，直接退回原来那张平铺列表：只有「未分组」
  /// 一组时，那个标题不传达任何信息，只是白占一行。
  static List<_Row> _rows(List<ChatSession> sessions, ProjectState projects) {
    if (!projects.showGrouping) {
      return [for (final s in sessions) _SessionRow(s)];
    }
    final rows = <_Row>[];
    for (final group in groupSessionsByProject(sessions, projects.projects)) {
      // 未分组那一组空着就整个不画：一个空的「未分组」标题是纯噪音
      if (group.isUngrouped && group.sessions.isEmpty) continue;
      rows.add(_HeaderRow(group));
      final id = group.projectId;
      if (id != null && projects.isCollapsed(id)) continue;
      rows.addAll(group.sessions.map(_SessionRow.new));
    }
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

  Future<void> _createProject(BuildContext context, WidgetRef ref) async {
    final name = await _askName(context, title: '新建项目', hint: '例如：Cortex 客户端');
    if (name == null || !context.mounted) return;
    await _run(
      context,
      () => ref.read(projectControllerProvider.notifier).create(name),
    );
  }

  Future<void> _renameProject(
    BuildContext context,
    WidgetRef ref,
    Project project,
  ) async {
    final name = await _askName(context, title: '重命名项目', initial: project.name);
    if (name == null || !context.mounted) return;
    await _run(
      context,
      () =>
          ref.read(projectControllerProvider.notifier).rename(project.id, name),
    );
  }

  /// 删除项目。
  ///
  /// 对话框正文由 [deleteProjectWarning] 写死，且**必须**说清会话不会丢：
  /// 用户在这里唯一真正害怕的事就是「删项目把对话也删了」，而那恰恰是唯一
  /// 不会发生的事。说不清楚的代价不是一次误操作，是从此没人敢碰这个功能。
  Future<void> _deleteProject(
    BuildContext context,
    WidgetRef ref,
    Project project,
  ) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('删除项目「${project.name}」？'),
        content: Text(deleteProjectWarning(project)),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            // 「删除项目」而不是「删除」：按钮上那个宾语是最后一次说明
            // 被删掉的到底是什么
            child: const Text('删除项目'),
          ),
        ],
      ),
    );
    if (ok != true || !context.mounted) return;
    await _run(
      context,
      () => ref.read(projectControllerProvider.notifier).remove(project.id),
    );
  }

  Future<String?> _askName(
    BuildContext context, {
    required String title,
    String? initial,
    String? hint,
  }) async {
    final field = TextEditingController(text: initial ?? '');
    final result = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: TextField(
          controller: field,
          autofocus: true,
          // 服务端 trim 之后限 100 字符。在这里就截住，是为了不让用户
          // 打完一长串再收到一条他看不出所以然的 400
          maxLength: 100,
          buildCounter:
              (_, {required currentLength, required isFocused, maxLength}) =>
                  null,
          decoration: InputDecoration(labelText: '项目名', hintText: hint),
          onSubmitted: (v) => Navigator.of(context).pop(v),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(field.text),
            child: const Text('保存'),
          ),
        ],
      ),
    );
    field.dispose();
    if (result == null || result.trim().isEmpty) return null;
    return result.trim();
  }

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

class _HeaderRow extends _Row {
  const _HeaderRow(this.group);

  final SessionGroup group;
}

class _SessionRow extends _Row {
  const _SessionRow(this.session);

  final ChatSession session;
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
    required this.onDelete,
  });

  final SessionGroup group;
  final bool collapsed;
  final bool selected;
  final VoidCallback onToggle;
  final VoidCallback onNewSession;
  final VoidCallback onRename;
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
                  onRename: onRename,
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
    required this.onRename,
    required this.onDelete,
  });

  /// 只用来构造那句 tooltip。会话行的菜单是空 tooltip（一列同样的图标，
  /// 每一个都弹一句话很吵），项目这一行只有一两个，说清是**哪个**项目
  /// 反而有用 —— 顺带让测试有一个稳定的抓手。
  final String projectName;
  final VoidCallback onRename;
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
        _ => onDelete(),
      },
      itemBuilder: (context) => const [
        PopupMenuItem(
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
        PopupMenuItem(
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
    required this.justFinished,
    required this.canMove,
    required this.onTap,
    required this.onRename,
    required this.onToggleArchive,
    required this.onMove,
    required this.onExport,
  });

  final ChatSession session;
  final bool selected;
  final bool streaming;

  /// 人不在的时候跑完了，还没被打开过。
  final bool justFinished;

  /// 这个部署有项目功能吗。没有的话「移动到项目」不进菜单 —— 一个点下去
  /// 只会报错的菜单项，比没有这个菜单项更糟。
  final bool canMove;
  final VoidCallback onTap;
  final VoidCallback onRename;
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
          child: Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        if (session.workspace != null) ...[
                          Tooltip(
                            message: '已绑定工作区：${session.workspace!.root}',
                            child: Icon(
                              Icons.folder_rounded,
                              size: 11,
                              color: scheme.secondary,
                            ),
                          ),
                          const SizedBox(width: 5),
                        ],
                        Expanded(
                          child: Text(
                            session.title,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: theme.textTheme.bodyMedium?.copyWith(
                              fontWeight: widget.selected
                                  ? FontWeight.w600
                                  : FontWeight.w400,
                              color: session.archived
                                  ? scheme.onSurface.withValues(alpha: 0.55)
                                  : widget.selected
                                  ? scheme.onSurface
                                  : scheme.onSurface.withValues(alpha: 0.88),
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
                                color: scheme.tertiary,
                              ),
                            ),
                          ),
                        ],
                      ],
                    ),
                  ],
                ),
              ),
              if (widget.streaming)
                Container(
                  width: 6,
                  height: 6,
                  margin: const EdgeInsets.only(right: 6),
                  decoration: BoxDecoration(
                    color: scheme.primary,
                    shape: BoxShape.circle,
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
                      color: scheme.primary,
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
                  onToggleArchive: widget.onToggleArchive,
                  onMove: widget.onMove,
                  onExport: widget.onExport,
                ),
              ),
            ],
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
    required this.onToggleArchive,
    required this.onMove,
    required this.onExport,
  });

  final bool archived;
  final bool enabled;
  final bool canMove;
  final VoidCallback onRename;
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
