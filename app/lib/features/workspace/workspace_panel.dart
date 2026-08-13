import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/local_agent.dart';
import '../../models/attachment.dart' show formatBytes;
import '../../models/workspace.dart';
import '../../state/chat_controller.dart';
import '../../workspace/workspace_fs.dart';
import 'sandbox_file_tree.dart';
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
    if (session == null) return const SizedBox.shrink();

    final workspace = session.workspace;
    final scheme = Theme.of(context).colorScheme;

    // 没有本地 agent 的构建（Web）：这块面板回答的是「我的文件在哪」，
    // 而在那儿答案与本机绑定**无关** —— 文件在云端容器的卷里。
    //
    // 所以它不能跟着 `workspace != null` 走。跟着走的后果实测过：Web 端绑
    // 工作区会被服务端 400 拒（任务 #75 卸掉了 cortexd 自己的文件工具），
    // 于是 workspace 恒为 null，整块面板连同**唯一的文件出入口**在 Web 上
    // 永远不出现 —— 而 Web 恰恰是唯一需要它的地方。
    final sandboxOnly = !kLocalAgentSupported;
    if (!sandboxOnly && workspace == null) return const SizedBox.shrink();

    return Container(
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: scheme.outlineVariant)),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (sandboxOnly)
            _Header(
              icon: Icons.folder_rounded,
              // 不叫「云沙箱」。用户这里要的是「我的文件在哪」，
              // 而「沙箱」是一个实现细节的名字 —— 它既不告诉他里面有什么，
              // 也让他以为需要先做点什么才能用
              title: '文件',
              tooltip: 'agent 读写的就是这些文件，跨会话保留',
              expanded: _expanded,
              onToggle: () => setState(() => _expanded = !_expanded),
            )
          else
            _Header(
              icon: Icons.folder_rounded,
              title: workspace!.displayName,
              tooltip: workspace.root,
              expanded: _expanded,
              onToggle: () => setState(() => _expanded = !_expanded),
              action: _HeaderAction(
                icon: Icons.swap_horiz_rounded,
                tooltip: '更换工作区',
                onPressed: () => showWorkspaceBindingSheet(context, session),
              ),
            ),
          if (_expanded)
            SizedBox(
              height: widget.maxTreeHeight,
              child: sandboxOnly
                  ? const SandboxWorkspaceView()
                  // Keyed by root so switching sessions to a differently-bound
                  // one discards the expansion set and cached listings of the
                  // old tree instead of showing them under the new root.
                  : _Tree(key: ValueKey(workspace!.root), root: workspace.root),
            ),
        ],
      ),
    );
  }
}

/// 一个可选的头部按钮。Web 那一支没有「更换工作区」可点 —— 那里的根是
/// 服务端定的，换不了。
class _HeaderAction {
  const _HeaderAction({
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onPressed;
}

class _Header extends StatelessWidget {
  const _Header({
    required this.icon,
    required this.title,
    required this.tooltip,
    required this.expanded,
    required this.onToggle,
    this.action,
  });

  final IconData icon;
  final String title;
  final String tooltip;
  final bool expanded;
  final VoidCallback onToggle;
  final _HeaderAction? action;

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
            Icon(icon, size: 14, color: scheme.secondary),
            const SizedBox(width: 7),
            Expanded(
              child: Tooltip(
                message: tooltip,
                child: Text(
                  title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelMedium?.copyWith(
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
            ),
            if (action case final action?)
              IconButton(
                onPressed: action.onPressed,
                iconSize: 15,
                visualDensity: VisualDensity.compact,
                tooltip: action.tooltip,
                icon: Icon(action.icon),
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

    // 这里不再有 Web 分支：没有本地 agent 的构建根本不会构造 `_Tree`
    // （见 `_WorkspacePanelState.build` 的 `sandboxOnly`），它拿到的是
    // 云沙箱那棵树。留一个永远画不出来的说明块，只会让下一个人以为
    // Web 端还停在「只有一段文字」那一版。
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
  });

  final IconData icon;
  final String title;
  final String detail;
  final Color? tone;

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
        ],
      ),
    );
  }
}

/// Shown in the chat header. Bound or not, this is where the workspace is
/// visible and changeable — the requirement being that a user never has to
/// wonder whether the agent can touch files.
///
/// # 只在有本地 agent 的构建里出现
///
/// 「绑定工作区」是**设备本地**的概念（任务 #37）：它说的是「agent 动你这台
/// 机器上的哪个目录」。Web 端没有本地 agent，那儿的 agent 在云端容器里，
/// 工作区是它自己的 `/workspace` —— 绑定这件事无从谈起，服务端也会 400 拒
/// （任务 #75 把文件与 shell 工具从 cortexd 卸掉了）。
///
/// 上一版没有这道判据，于是 Web 端顶栏一直挂着一个「绑定工作区」，
/// 提示语写着「这是一个纯聊天会话，助手拿不到文件工具」—— **两句都是错的**：
/// Web 端的 agent 有全套文件工具，而点下去只会得到一个 400。
///
/// 判据是**能力**而不是 `kIsWeb`，与 `WorkspacePanel`、`SandboxWorkspaceView`
/// 用的是同一个（见 `core/local_agent.dart`）。
class WorkspaceChip extends ConsumerWidget {
  const WorkspaceChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (!kLocalAgentSupported) return const SizedBox.shrink();

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
