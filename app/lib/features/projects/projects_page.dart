/// 项目 —— 一面卡片墙。
///
/// # 为什么它值一个自己的地方
///
/// 在这一页之前，项目只是会话列表里的一个分组标题：没置顶的那些**没有
/// 任何入口**，你得先记得它叫什么，再在一列会话里找那一行。而项目本来是
/// 「一摊活儿」，不是「一条会话的属性」。
///
/// # 与图片页同一副骨架
///
/// `PanelHeader` + `SliverGrid`，逐字照着 `image_page.dart` 那一套。
/// 两个「地方」长得不一样的话，用户每进一个都要重新学一遍它的顶栏在哪儿。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/project.dart';
import '../../state/app_providers.dart';
import '../../state/project_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import 'project_actions.dart';

class ProjectsPage extends ConsumerWidget {
  const ProjectsPage({
    super.key,
    this.onToggleSessions,
    this.sessionsVisible = false,
  });

  /// 收起 / 展开左栏。窄屏上 `AppShell` 换成「开抽屉」。
  final VoidCallback? onToggleSessions;
  final bool sessionsVisible;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(projectControllerProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '项目',
          subtitle: state.projects.isEmpty ? null : '置顶的会出现在左栏',
          leading: onToggleSessions == null
              ? null
              : IconButton(
                  onPressed: onToggleSessions,
                  iconSize: 19,
                  tooltip: sessionsVisible ? '隐藏会话栏' : '显示会话栏',
                  icon: Icon(
                    sessionsVisible
                        ? Icons.menu_open_rounded
                        : Icons.menu_rounded,
                  ),
                ),
          actions: [
            if (!state.unsupported)
              TextButton.icon(
                key: const ValueKey('projects:new'),
                onPressed: () => createProject(context, ref),
                icon: const Icon(Icons.create_new_folder_outlined, size: 16),
                label: const Text('新建项目'),
              ),
          ],
        ),
        Expanded(child: _body(context, ref, state)),
      ],
    );
  }

  Widget _body(BuildContext context, WidgetRef ref, ProjectState state) {
    if (state.unsupported) {
      return const EmptyState(
        icon: Icons.folder_off_outlined,
        title: '这个后端没有项目',
        // 说清是**后端**旧了，不是出错了 —— 重试永远不会成功。
        // 与画廊那一页的措辞刻意一致
        description: '它是一个老版本的部署，还没有 /projects 这条路。升级之后这里会自己出现。',
      );
    }
    if (state.loading && state.projects.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }
    if (state.projects.isEmpty) {
      return EmptyState(
        icon: Icons.folder_outlined,
        title: '还没有项目',
        description: '项目是一摊活儿的容器 —— 把相关的对话归到一起。删项目不会删对话。',
        action: FilledButton.icon(
          onPressed: () => createProject(context, ref),
          icon: const Icon(Icons.create_new_folder_outlined, size: 18),
          label: const Text('新建项目'),
        ),
      );
    }

    return CustomScrollView(
      slivers: [
        SliverPadding(
          padding: const EdgeInsets.fromLTRB(20, 16, 20, 24),
          // 与图库同一个理由：`shrinkWrap` 会把整页卡片全建出来。
          // 项目数量目前撑不起这个差别，但两处形状一致比省几行重要
          sliver: SliverGrid.builder(
            gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
              maxCrossAxisExtent: 420,
              crossAxisSpacing: 12,
              mainAxisSpacing: 12,
              // 卡片是一行标题 + 一行元信息，不需要方的
              mainAxisExtent: 104,
            ),
            itemCount: state.projects.length,
            itemBuilder: (context, i) => _ProjectCard(
              key: ValueKey('project:${state.projects[i].id}'),
              project: state.projects[i],
            ),
          ),
        ),
      ],
    );
  }
}

class _ProjectCard extends ConsumerWidget {
  const _ProjectCard({super.key, required this.project});

  final Project project;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Material(
      color: scheme.surfaceContainerLow,
      clipBehavior: Clip.antiAlias,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        side: BorderSide(color: scheme.outlineVariant),
      ),
      child: InkWell(
        // 点卡片 = 去看这个项目里的对话。**不是打开一个项目详情页** ——
        // 项目里除了会话什么都没有，再套一层只是多一次点击
        onTap: () {
          ref.read(projectControllerProvider.notifier)
            ..select(project.id)
            ..expand(project.id);
          ref.read(mainViewProvider.notifier).go(MainView.chat);
        },
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 6, 12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Flexible(
                    child: Text(
                      project.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleSmall,
                    ),
                  ),
                  // 置顶的**一眼看得见**：否则用户点了置顶之后，除了左栏
                  // 多一行以外，这一页上没有任何地方能告诉他状态变了
                  if (project.pinned) ...[
                    const SizedBox(width: 6),
                    Icon(
                      Icons.push_pin,
                      size: 13,
                      key: const ValueKey('project:pinned'),
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ],
                  const Spacer(),
                  _ProjectCardMenu(project: project),
                ],
              ),
              const Spacer(),
              Text(
                // 时间解析不出来就不画 —— 编一个「刚刚」比留白更糟
                [
                  '${project.sessionCount} 个对话',
                  if (project.createdAt case final at?) _day(at),
                ].join(' · '),
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  static String _day(DateTime at) =>
      '${at.year}-${at.month.toString().padLeft(2, '0')}-'
      '${at.day.toString().padLeft(2, '0')}';
}

class _ProjectCardMenu extends ConsumerWidget {
  const _ProjectCardMenu({required this.project});

  final Project project;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return PopupMenuButton<String>(
      key: ValueKey('project:menu:${project.id}'),
      tooltip: '「${project.name}」的更多操作',
      iconSize: 16,
      icon: const Icon(Icons.more_vert_rounded),
      onSelected: (value) => switch (value) {
        'pin' => setProjectPinned(context, ref, project, !project.pinned),
        'rename' => renameProject(context, ref, project),
        _ => deleteProject(context, ref, project),
      },
      itemBuilder: (context) => [
        PopupMenuItem(
          value: 'pin',
          height: 38,
          child: Row(
            children: [
              Icon(
                project.pinned
                    ? Icons.push_pin_outlined
                    : Icons.push_pin_rounded,
                size: 15,
              ),
              const SizedBox(width: 9),
              Text(project.pinned ? '取消置顶' : '置顶到左栏'),
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
              // 敢叫「删除」是因为删的真的只是这层分组 —— 会话一条不少
              Text('删除项目'),
            ],
          ),
        ),
      ],
    );
  }
}
