import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../core/formatting.dart';
import '../../models/chat_session.dart';
import '../../state/chat_controller.dart';
import '../../state/chat_state.dart';
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

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '会话',
          actions: [
            _ArchivedToggle(
              value: state.showArchived,
              onChanged: controller.setShowArchived,
            ),
            IconButton(
              onPressed: () {
                controller.createSession();
                onSelected?.call();
              },
              iconSize: 18,
              tooltip: '新建会话',
              icon: const Icon(Icons.add_rounded),
            ),
          ],
        ),
        Expanded(child: _body(context, ref, state, controller)),
      ],
    );
  }

  Widget _body(
    BuildContext context,
    WidgetRef ref,
    ChatState state,
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
    if (sessions.isEmpty) {
      return EmptyState(
        icon: Icons.forum_outlined,
        title: state.showArchived ? '还没有会话' : '没有未归档的会话',
        description: state.showArchived
            ? null
            : '打开右上角的归档开关可以看到已归档的会话。',
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

    return ListView.builder(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 8),
      itemCount: sessions.length,
      itemBuilder: (context, i) {
        final session = sessions[i];
        return _SessionTile(
          key: ValueKey(session.id),
          session: session,
          selected: session.id == state.activeSessionId,
          streaming: state.streaming?.sessionId == session.id,
          onTap: () {
            controller.selectSession(session.id);
            onSelected?.call();
          },
          onRename: () => _rename(context, ref, session),
          onToggleArchive: () =>
              _archive(context, ref, session, !session.archived),
        );
      },
    );
  }

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
            helperText: session.titleIsCustom
                ? null
                : '留空则继续沿用首条消息派生的标题',
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
  /// nobody awaits. An *unsupported* endpoint never reaches here — the
  /// controller absorbs that case and marks the session as locally edited.
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
    required this.onTap,
    required this.onRename,
    required this.onToggleArchive,
  });

  final ChatSession session;
  final bool selected;
  final bool streaming;
  final VoidCallback onTap;
  final VoidCallback onRename;
  final VoidCallback onToggleArchive;

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

    final Color background;
    if (widget.selected) {
      background = scheme.primary.withValues(alpha: 0.12);
    } else if (_hovered) {
      background = scheme.surfaceContainerHigh;
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
          duration: const Duration(milliseconds: 110),
          margin: const EdgeInsets.only(bottom: 2),
          padding: const EdgeInsets.fromLTRB(11, 9, 4, 9),
          decoration: BoxDecoration(
            color: background,
            borderRadius: BorderRadius.circular(8),
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
                ),
              // Kept mounted rather than built on hover: a menu that appears
              // under the cursor shifts the row's layout the moment the pointer
              // arrives, which makes the title jitter as the mouse crosses it.
              Opacity(
                opacity: _hovered || widget.selected ? 1 : 0,
                child: _TileMenu(
                  archived: session.archived,
                  enabled: _hovered || widget.selected,
                  onRename: widget.onRename,
                  onToggleArchive: widget.onToggleArchive,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _TileMenu extends StatelessWidget {
  const _TileMenu({
    required this.archived,
    required this.enabled,
    required this.onRename,
    required this.onToggleArchive,
  });

  final bool archived;
  final bool enabled;
  final VoidCallback onRename;
  final VoidCallback onToggleArchive;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<String>(
      enabled: enabled,
      tooltip: '',
      iconSize: 15,
      padding: EdgeInsets.zero,
      splashRadius: 14,
      icon: const Icon(Icons.more_horiz_rounded),
      onSelected: (value) => switch (value) {
        'rename' => onRename(),
        _ => onToggleArchive(),
      },
      itemBuilder: (context) => [
        const PopupMenuItem(
          value: 'rename',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.edit_outlined, size: 15),
              SizedBox(width: 9),
              Text('重命名'),
            ],
          ),
        ),
        PopupMenuItem(
          value: 'archive',
          height: 38,
          child: Row(
            children: [
              Icon(
                archived
                    ? Icons.unarchive_outlined
                    : Icons.archive_outlined,
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
      ],
    );
  }
}
