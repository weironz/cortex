import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../core/local_agent.dart';
import '../../models/attachment.dart' show formatBytes;
import '../../models/chat_session.dart';
import '../../models/workspace.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../widgets/panel_header.dart';
import '../../workspace/workspace_fs.dart';
import 'cloud_workspace_sheet.dart';
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
class WorkspacePanel extends ConsumerWidget {
  const WorkspacePanel({super.key, this.onClose});

  /// 关闭这一栏。与 `MemoryPanel({this.onClose})` 同形 —— 右栏里两个面板
  /// 轮流住，它们对外的形状必须一样，否则 `AppShell` 那个 `switch`
  /// 要为每一支写不同的接线。
  final VoidCallback? onClose;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final session = ref.watch(
      chatControllerProvider.select((s) => s.activeSession),
    );

    final workspace = session?.workspace;
    final scheme = Theme.of(context).colorScheme;

    // 没有本地 agent 的构建（Web）：这块面板回答的是「我的文件在哪」，
    // 而在那儿答案与本机绑定**无关** —— 文件在云端容器的卷里。
    //
    // 所以它不能跟着 `workspace != null` 走。跟着走的后果实测过：Web 端绑
    // 工作区会被服务端 400 拒（任务 #75 卸掉了 cortexd 自己的文件工具），
    // 于是 workspace 恒为 null，整块面板连同**唯一的文件出入口**在 Web 上
    // 永远不出现 —— 而 Web 恰恰是唯一需要它的地方。
    final sandboxOnly = !kLocalAgentSupported;

    // 没有会话、或者桌面端这个会话还没绑目录 —— **没东西可画，但栏还得在**。
    //
    // 搬到右栏之前这两处是 `SizedBox.shrink()`：那时它只是左栏底下的一块，
    // 消失就消失了。现在它是**整整一列**，画不出东西就等于一片空白，
    // 而且连关闭按钮都没有 —— 用户打开一个关不掉的空栏。
    final empty = session == null
        ? '还没选中会话。'
        : (!sandboxOnly && workspace == null
              ? '这个会话还没绑定目录 —— 点上面那个换向箭头选一个。'
              : null);

    // 面板自己那层折叠没了：右栏整个由顶栏那个图标控制，再套一层
    // 就是两个开关管同一件事 —— 而用户永远说不清该点哪个
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          // 不叫「云沙箱」。用户这里要的是「我的文件在哪」，而「沙箱」
          // 是一个实现细节的名字 —— 它既不告诉他里面有什么，也让他以为
          // 需要先做点什么才能用
          title: sandboxOnly
              ? (session?.containerWorkspace ?? '文件')
              : (workspace?.displayName ?? '文件'),
          subtitle: sandboxOnly
              ? (session?.containerWorkspace == null
                    ? 'agent 读写的就是这些，跨会话保留'
                    // 路径写全：用户点开选择器之前，得先看得出自己在哪一层
                    : '/workspace/${session!.containerWorkspace}')
              : workspace?.root,
          leading: Icon(
            Icons.folder_rounded,
            size: 15,
            color: scheme.secondary,
          ),
          actions: [
            // 两支各有一个「更换工作区」，**同一个图标、同一个位置**，
            // 但打开的是两个不同的界面 —— 因为它们问的不是同一个问题：
            // 桌面端问「用这台机器上的哪个目录」，云端问「用那个卷里的
            // 哪个子目录」。
            //
            // 云端这一支从前是没有的，注释里写着「那儿的根是服务端定的」。
            // 那句话在 0.1.11 之后就不成立了（会话可以把根收窄到
            // `/workspace/<名字>`），只是能力做完之后**没人回来接界面** ——
            // 于是它在 HTTP 上活了一版，用户点不到。
            if (session != null)
              IconButton(
                onPressed: () => sandboxOnly
                    ? showCloudWorkspaceSheet(context, session)
                    : showWorkspaceBindingSheet(context, session),
                iconSize: 17,
                tooltip: '更换工作区',
                icon: const Icon(Icons.swap_horiz_rounded),
              ),
            if (onClose != null)
              IconButton(
                onPressed: onClose,
                iconSize: 17,
                tooltip: '关闭',
                icon: const Icon(Icons.close_rounded),
              ),
          ],
        ),
        // 整列之后从 `SizedBox(height:)` 改成 `Expanded` —— 这是搬家唯一的
        // 真收益：文件多的时候不再被那个 320px 的框卡住
        Expanded(
          child: empty != null
              ? Center(
                  child: Padding(
                    padding: const EdgeInsets.all(24),
                    child: Text(
                      empty,
                      textAlign: TextAlign.center,
                      style: Theme.of(context).textTheme.labelSmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                )
              : sandboxOnly
              ? const SandboxWorkspaceView()
              // Keyed by root so switching sessions to a differently-bound
              // one discards the expansion set and cached listings of the
              // old tree instead of showing them under the new root.
              : _Tree(key: ValueKey(workspace!.root), root: workspace.root),
        ),
      ],
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
    final cloud = ref
        .read(chatControllerProvider.notifier)
        .isCloudByChoice(session.id);

    final (IconData icon, String label, String tip, Color color) = switch ((
      workspace,
      cloud,
    )) {
      (final w?, _) => (
        Icons.folder_rounded,
        w.displayName,
        '这次对话的文件都在 ${w.root}\n点击更换',
        scheme.secondary,
      ),
      // 还没定：草稿会在第一句话之前自动开一个按日期时间命名的文件夹，
      // 而**已经开过口**的会话不会 —— 那时说「新建工作区」就是句空话
      (null, false) when session.isLocalDraft => (
        Icons.auto_awesome_outlined,
        '新建工作区',
        '发出第一句话时，会在默认工作空间下建一个以当前时间命名的文件夹。\n点击改成别的。',
        scheme.onSurfaceVariant,
      ),
      (null, _) => (
        Icons.cloud_outlined,
        '云端',
        '这次对话跑在远端 agent 的容器里，任何设备上都能接着聊。\n点击改成本机目录。',
        scheme.onSurfaceVariant,
      ),
    };

    return Tooltip(
      message: tip,
      child: Material(
        color: color.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(7),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => _pick(context, ref, session),
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
                Icon(icon, size: 13, color: color),
                const SizedBox(width: 6),
                Flexible(
                  child: Text(
                    label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: color,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
                Icon(Icons.arrow_drop_down_rounded, size: 15, color: color),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Future<void> _pick(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
  ) async {
    final root = await ref.read(localWorkspaceRootProvider.future);
    if (!context.mounted) return;
    final choice = await showModalBottomSheet<_WorkspaceChoice>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => _WorkspaceSheet(session: session, root: root),
    );
    if (choice == null || !context.mounted) return;
    if (!await _confirmSwitch(context, ref, session, choice)) return;
    if (!context.mounted) return;

    final controller = ref.read(chatControllerProvider.notifier);
    try {
      switch (choice) {
        case _Cloud():
          await controller.chooseCloud(session.id);
        case _Auto():
          // 「新建工作区」= 清掉现在这个绑定，让首轮那一步重新决定。
          // 它只对草稿有意义，所以清单里也只对草稿显示
          await controller.unbindWorkspace(session.id);
        // 已有的和新建的走同一条：那个端点本来就是「给我这个名字的工作空间
        // 目录，没有就建一个」，分成两条只会多一处能漂开的语义
        case _Folder(:final name) || _NewFolder(:final name):
          await controller.createLocalWorkspace(session.id, name);
        case _Browse():
          if (context.mounted) {
            await showWorkspaceBindingSheet(context, session);
          }
      }
    } on CortexApiException catch (e) {
      if (!context.mounted) return;
      // 校验器的话是写给人看的（「工作空间名里不能有路径分隔符」），原样显示
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(e.message)));
    }
    // 刚建出来的文件夹要出现在下一次的清单里
    ref.invalidate(localWorkspaceRootProvider);
  }

  /// 已经开过口的会话换工作区，问一次。
  ///
  /// # 为什么是「问一次」而不是「不许换」
  ///
  /// 调研过的四家都把工作区钉死在会话开始那一刻（Claude Code / Codex 靠
  /// 启动参数，只能 `/add-dir` **追加**；Cursor 的工作区就是窗口；
  /// OpenHands 的 runtime 绑在 conversation 上）。理由成立，但一刀切会
  /// 挡住一件正当的事：绑错了、还没干正事，逼人重开一个会话。
  ///
  /// 所以分界线不是「开没开聊」，是**有没有东西依赖旧工作区**。没有轮次
  /// 就没有依赖，零阻力；有轮次就把后果摆出来让人自己判断。
  ///
  /// # 换过去之后仍然留着的那个坑
  ///
  /// 模型上下文里那些「我读过 / 写过 X」的印象**不会**被告知已经换了世界。
  /// 这一层做不到，它得由服务端把「工作区变更」也写进那一轮的上下文。
  /// 这句话因此写进了弹层正文 —— 说不出口的限制，至少要说得出口。
  static Future<bool> _confirmSwitch(
    BuildContext context,
    WidgetRef ref,
    ChatSession session,
    _WorkspaceChoice choice,
  ) async {
    // 草稿的 messageCount 还是 0（服务端没听说过它），所以两边都看：
    // 只认一边的话，「刚聊完第一句就换」会静默滑过去
    final turns = ref.read(chatControllerProvider).activeTranscript.length;
    if (turns == 0 && session.messageCount == 0) return true;

    final from = session.workspace?.root ?? '云端容器';
    final to = switch (choice) {
      _Cloud() => '云端容器',
      _Auto() => '一个新建的工作区',
      _Folder(:final name) || _NewFolder(:final name) => name,
      _Browse() => '你接下来选的那个目录',
    };
    if (from == to) return true;

    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        icon: const Icon(Icons.swap_horiz_rounded),
        title: const Text('换掉这个会话的工作区？'),
        content: Text(
          '这个会话已经聊过了，之前那些轮次跑在 $from。\n\n'
          '换到 $to 之后，模型上下文里还记着旧那边的文件 —— '
          '它会照着旧印象去动新目录里的同名文件，而它不会知道换过。\n\n'
          '历史里那些路径也跟着变了意思：同一个 src/foo.rs '
          '指的已经是另一个文件。',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('算了'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('换过去'),
          ),
        ],
      ),
    );
    return ok ?? false;
  }
}

/// 选择器的结果。
///
/// 用 sealed 而不是「一个 String 标签 + 一个可空 payload」：后者会让消费点
/// 判一次「这个标签下 payload 该不该有值」，而判漏的那一支是运行时才炸。
sealed class _WorkspaceChoice {
  const _WorkspaceChoice();
}

class _Auto extends _WorkspaceChoice {
  const _Auto();
}

class _Cloud extends _WorkspaceChoice {
  const _Cloud();
}

/// 默认根目录下已有的一个文件夹。
class _Folder extends _WorkspaceChoice {
  const _Folder(this.name);
  final String name;
}

class _NewFolder extends _WorkspaceChoice {
  const _NewFolder(this.name);
  final String name;
}

/// 去选任意一个目录（默认根目录之外的也行）。
class _Browse extends _WorkspaceChoice {
  const _Browse();
}

/// 按日期时间自动建出来的那种文件夹名。
///
/// 它们**不进选择清单**：每聊一次就多一个，一周之后清单里九成是它们，
/// 而它们是某一次对话的落地目录，不是用户会特意回去的工作空间。
/// 真要回去的话，「选择其他文件夹」那条路照样选得到。
final _autoNamed = RegExp(r'^\d{4}-\d{2}-\d{2}-\d{2}-\d{2}-\d{2}$');

class _WorkspaceSheet extends StatelessWidget {
  const _WorkspaceSheet({required this.session, required this.root});

  final ChatSession session;
  final LocalWorkspaceRoot root;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final named = root.folders.where((f) => !_autoNamed.hasMatch(f)).toList();
    final current = session.workspace?.root;

    return SafeArea(
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (session.isLocalDraft)
              ListTile(
                leading: const Icon(Icons.auto_awesome_outlined),
                title: const Text('新建工作区'),
                subtitle: Text(
                  root.root == null
                      ? '在默认工作空间下建一个以当前时间命名的文件夹'
                      : '在 ${root.root} 下建一个以当前时间命名的文件夹',
                ),
                trailing: current == null
                    ? const Icon(Icons.check_rounded)
                    : null,
                onTap: () => Navigator.of(context).pop(const _Auto()),
              ),
            ListTile(
              leading: const Icon(Icons.cloud_outlined),
              title: const Text('云端'),
              subtitle: const Text('跑在远端 agent 的容器里，任何设备上都能接着聊'),
              onTap: () => Navigator.of(context).pop(const _Cloud()),
            ),
            if (named.isNotEmpty) ...[
              const Divider(height: 1),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: Text('默认工作空间下已有的', style: theme.textTheme.labelSmall),
                ),
              ),
              for (final name in named)
                ListTile(
                  dense: true,
                  leading: const Icon(Icons.folder_outlined),
                  title: Text(name),
                  trailing: current != null && basenameOf(current) == name
                      ? const Icon(Icons.check_rounded)
                      : null,
                  onTap: () => Navigator.of(context).pop(_Folder(name)),
                ),
            ],
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.create_new_folder_outlined),
              title: const Text('新建工作空间…'),
              onTap: () async {
                final name = await _askName(context);
                if (name != null && context.mounted) {
                  Navigator.of(context).pop(_NewFolder(name));
                }
              },
            ),
            ListTile(
              leading: const Icon(Icons.folder_open_outlined),
              title: const Text('选择其他文件夹…'),
              subtitle: const Text('默认工作空间之外的任意目录'),
              onTap: () => Navigator.of(context).pop(const _Browse()),
            ),
          ],
        ),
      ),
    );
  }

  /// 只问名字，不问路径 —— 工作空间就是默认根目录下的一个同名文件夹，
  /// 让用户再填一次父目录等于把那个设置项的意义抵消掉。
  static Future<String?> _askName(BuildContext context) {
    final controller = TextEditingController();
    return showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('新建工作空间'),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: const InputDecoration(
            labelText: '名称',
            hintText: '比如：季度汇报',
          ),
          onSubmitted: (v) => Navigator.of(ctx).pop(v.trim()),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(controller.text.trim()),
            child: const Text('创建'),
          ),
        ],
      ),
    ).then((v) => (v == null || v.isEmpty) ? null : v);
  }
}
