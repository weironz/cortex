import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../models/attachment.dart' show formatBytes;
import '../../models/workspace.dart';
import '../../state/chat_controller.dart';
import '../../workspace/workspace_fs.dart';
import 'sandbox_download_button.dart';
import 'workspace_binding_sheet.dart';

/// The collapsible file tree that appears once a session is bound.
///
/// Read-only on purpose. The point is orientation — "which directory is the
/// agent actually pointed at, and does it contain what I think it does" — not
/// editing. An editor here would be a second, worse editor next to the one the
/// user already has open, and it would need write conflict handling with the
/// agent that is writing to the same files.
///
/// The tree is lazy: one directory level per expand. Eagerly walking a
/// repository root would stall on `node_modules` or `target` for seconds, and
/// the first thing the user wants to see is the top level anyway.
class WorkspacePanel extends ConsumerStatefulWidget {
  const WorkspacePanel({super.key, this.maxTreeHeight = 300});

  /// Height given to the tree when open.
  ///
  /// Passed in rather than taken with an `Expanded`: this panel is the bottom
  /// half of a `Column` whose top half is the session list, and both cannot
  /// claim the remaining space. The parent measures once and splits.
  final double maxTreeHeight;

  @override
  ConsumerState<WorkspacePanel> createState() => _WorkspacePanelState();
}

class _WorkspacePanelState extends ConsumerState<WorkspacePanel> {
  bool _expanded = true;

  @override
  Widget build(BuildContext context) {
    final session = ref.watch(
      chatControllerProvider.select((s) => s.activeSession),
    );
    final workspace = session?.workspace;
    if (session == null || workspace == null) return const SizedBox.shrink();

    final scheme = Theme.of(context).colorScheme;

    return Container(
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: scheme.outlineVariant)),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Header(
            workspace: workspace,
            expanded: _expanded,
            onToggle: () => setState(() => _expanded = !_expanded),
            onRebind: () => showWorkspaceBindingSheet(context, session),
          ),
          if (_expanded)
            SizedBox(
              height: widget.maxTreeHeight,
              // Keyed by root so switching sessions to a differently-bound one
              // discards the expansion set and cached listings of the old tree
              // instead of showing them under the new root.
              child: _Tree(key: ValueKey(workspace.root), root: workspace.root),
            ),
        ],
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.workspace,
    required this.expanded,
    required this.onToggle,
    required this.onRebind,
  });

  final Workspace workspace;
  final bool expanded;
  final VoidCallback onToggle;
  final VoidCallback onRebind;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return InkWell(
      onTap: onToggle,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(10, 8, 6, 8),
        child: Row(
          children: [
            AnimatedRotation(
              turns: expanded ? 0 : -0.25,
              duration: const Duration(milliseconds: 150),
              child: Icon(
                Icons.expand_more_rounded,
                size: 17,
                color: scheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(width: 4),
            Icon(Icons.folder_rounded, size: 14, color: scheme.secondary),
            const SizedBox(width: 7),
            Expanded(
              child: Tooltip(
                message: workspace.root,
                child: Text(
                  workspace.displayName,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelMedium?.copyWith(
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
            ),
            IconButton(
              onPressed: onRebind,
              iconSize: 15,
              visualDensity: VisualDensity.compact,
              tooltip: '更换工作区',
              icon: const Icon(Icons.swap_horiz_rounded),
            ),
          ],
        ),
      ),
    );
  }
}

class _Tree extends StatefulWidget {
  const _Tree({super.key, required this.root});

  final String root;

  @override
  State<_Tree> createState() => _TreeState();
}

class _TreeState extends State<_Tree> {
  /// Absolute paths of every expanded directory. Keyed by path rather than by
  /// index so the set survives a sibling being added or removed.
  final Set<String> _open = {};

  /// One entry per directory that has been listed at least once.
  final Map<String, List<FileNode>> _children = {};
  final Set<String> _loading = {};
  final Map<String, String> _errors = {};

  bool? _rootExists;

  @override
  void initState() {
    super.initState();
    _checkRoot();
  }

  Future<void> _checkRoot() async {
    if (!kCanBrowseLocalFiles) {
      setState(() => _rootExists = true);
      return;
    }
    final exists = await workspaceExists(widget.root);
    if (!mounted) return;
    setState(() => _rootExists = exists);
    if (exists) _load(widget.root);
  }

  void _load(String path) {
    if (_children.containsKey(path) || _loading.contains(path)) return;
    setState(() => _loading.add(path));
    listWorkspaceDirectory(path)
        .then((nodes) {
          if (!mounted) return;
          setState(() {
            _loading.remove(path);
            _children[path] = nodes;
            _errors.remove(path);
          });
        })
        .catchError((Object e) {
          if (!mounted) return;
          setState(() {
            _loading.remove(path);
            _errors[path] = '$e';
          });
        });
  }

  void _toggle(String path) {
    setState(() {
      if (_open.remove(path)) return;
      _open.add(path);
    });
    _load(path);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    // Web: no local filesystem, and the reason is a product statement rather
    // than an error, so it gets neutral styling.
    //
    // 这段话改过一次。原文说「工作区已绑定并对 agent 生效」—— 那是任务 #75
    // 之前的事实：那时 cortexd 自己带文件工具。#75 把它们卸掉之后，Web 端绑
    // 工作区会被 400 拒，而这行字还在说已经生效，把「功能不存在」说成了
    // 「功能在，只是你看不见」。
    if (!kCanBrowseLocalFiles) {
      return _Notice(
        icon: Icons.public_rounded,
        title: 'Web 端没有本机文件',
        detail:
            '浏览器读不到你这台机器的磁盘，绑本机工作区在 Web 端也不生效。'
            '要让 agent 读写文件，在输入框打开「云沙箱」—— '
            '它的工作区是云端容器里的 /workspace，跨会话保留。',
        // 云沙箱唯一的文件出口。放在这里是因为这块面板本来就是用户
        // 「我的文件在哪」的落点 —— 而在 Web 上那个答案是「在容器里，
        // 从这儿拿出来」
        trailing: const SandboxDownloadButton(),
      );
    }

    if (_rootExists == null) {
      return const Center(
        child: SizedBox(
          width: 15,
          height: 15,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    if (_rootExists == false) {
      // The binding syncs across devices; the directory does not.
      return _Notice(
        icon: Icons.folder_off_outlined,
        title: '这台机器上没有这个目录',
        detail:
            '${widget.root}\n\n'
            '工作区绑定会跟着会话同步到别的设备，而路径是本机的。'
            '换一个本机存在的目录即可。',
        tone: scheme.error,
      );
    }

    final rootChildren = _children[widget.root];
    if (rootChildren == null) {
      return Center(
        child: _errors[widget.root] == null
            ? const SizedBox(
                width: 15,
                height: 15,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : Padding(
                padding: const EdgeInsets.all(14),
                child: Text(
                  _errors[widget.root]!,
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.error,
                  ),
                ),
              ),
      );
    }

    // The tree is flattened into a single list so one `ListView.builder`
    // virtualises the whole thing — a nested `Column` per directory would build
    // every descendant of every open node, which is the cost the lazy listing
    // was meant to avoid in the first place.
    final rows = <_Row>[];
    void walk(String path, int depth) {
      for (final node in _children[path] ?? const <FileNode>[]) {
        rows.add(_Row(node: node, depth: depth));
        if (node.isDirectory && _open.contains(node.path)) {
          if (_loading.contains(node.path)) {
            rows.add(_Row(node: node, depth: depth + 1, placeholder: true));
          } else {
            walk(node.path, depth + 1);
          }
        }
      }
    }

    walk(widget.root, 0);

    if (rows.isEmpty) {
      return Center(child: Text('目录是空的', style: theme.textTheme.labelSmall));
    }

    return Scrollbar(
      child: ListView.builder(
        padding: const EdgeInsets.only(bottom: 8),
        itemCount: rows.length,
        itemExtent: 24,
        itemBuilder: (context, i) {
          final row = rows[i];
          if (row.placeholder) {
            return Padding(
              padding: EdgeInsets.only(left: 14.0 + row.depth * 13, top: 6),
              child: SizedBox(
                width: 11,
                height: 11,
                child: CircularProgressIndicator(
                  strokeWidth: 1.5,
                  color: scheme.onSurfaceVariant,
                ),
              ),
            );
          }
          return _NodeRow(
            node: row.node,
            depth: row.depth,
            expanded: _open.contains(row.node.path),
            onTap: row.node.isDirectory ? () => _toggle(row.node.path) : null,
          );
        },
      ),
    );
  }
}

class _Row {
  const _Row({
    required this.node,
    required this.depth,
    this.placeholder = false,
  });

  final FileNode node;
  final int depth;
  final bool placeholder;
}

class _NodeRow extends StatelessWidget {
  const _NodeRow({
    required this.node,
    required this.depth,
    required this.expanded,
    this.onTap,
  });

  final FileNode node;
  final int depth;
  final bool expanded;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return InkWell(
      onTap: onTap,
      child: Padding(
        padding: EdgeInsets.only(left: 10.0 + depth * 13, right: 8),
        child: Row(
          children: [
            SizedBox(
              width: 14,
              child: node.isDirectory
                  ? Icon(
                      expanded
                          ? Icons.expand_more_rounded
                          : Icons.chevron_right_rounded,
                      size: 14,
                      color: scheme.onSurfaceVariant,
                    )
                  : null,
            ),
            Icon(
              node.isDirectory
                  ? Icons.folder_outlined
                  : Icons.description_outlined,
              size: 13,
              color: node.isDirectory
                  ? scheme.secondary
                  : scheme.onSurfaceVariant,
            ),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                node.name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurface.withValues(alpha: 0.9),
                ),
              ),
            ),
            if (!node.isDirectory && node.sizeBytes != null)
              Text(
                formatBytes(node.sizeBytes),
                style: theme.textTheme.labelSmall?.copyWith(fontSize: 9),
              ),
          ],
        ),
      ),
    );
  }
}

class _Notice extends StatelessWidget {
  const _Notice({
    required this.icon,
    required this.title,
    required this.detail,
    this.tone,
    this.trailing,
  });

  final IconData icon;
  final String title;
  final String detail;
  final Color? tone;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final color = tone ?? scheme.onSurfaceVariant;

    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(14, 14, 14, 18),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(icon, size: 15, color: color),
              const SizedBox(width: 7),
              Expanded(
                child: Text(
                  title,
                  style: theme.textTheme.labelMedium?.copyWith(color: color),
                ),
              ),
            ],
          ),
          const SizedBox(height: 6),
          Text(detail, style: theme.textTheme.labelSmall),
          if (trailing != null) ...[
            const SizedBox(height: 12),
            Align(alignment: Alignment.centerLeft, child: trailing),
          ],
        ],
      ),
    );
  }
}

/// Shown in the chat header. Bound or not, this is where the workspace is
/// visible and changeable — the requirement being that a user never has to
/// wonder whether the agent can touch files.
class WorkspaceChip extends ConsumerWidget {
  const WorkspaceChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final session = ref.watch(
      chatControllerProvider.select((s) => s.activeSession),
    );
    if (session == null) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final workspace = session.workspace;
    final bound = workspace != null;

    final color = bound ? scheme.secondary : scheme.onSurfaceVariant;

    return Tooltip(
      message: bound
          ? '工作区：${workspace.root}\n点击更换或解除绑定'
          : '未绑定工作区 —— 这是一个纯聊天会话，助手拿不到文件工具。点击绑定。',
      child: Material(
        color: color.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(7),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => showWorkspaceBindingSheet(context, session),
          child: Container(
            constraints: const BoxConstraints(maxWidth: 200),
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(7),
              border: Border.all(color: color.withValues(alpha: 0.32)),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  bound
                      ? Icons.folder_rounded
                      : Icons.create_new_folder_outlined,
                  size: 13,
                  color: color,
                ),
                const SizedBox(width: 6),
                Flexible(
                  child: Text(
                    bound ? workspace.displayName : '绑定工作区',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: color,
                      fontWeight: FontWeight.w600,
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
