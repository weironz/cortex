/// 云沙箱工作区的**文件树 + 上传入口**。
///
/// # 为什么要有
///
/// 在这之前，Web 端用户与容器里那个卷之间只有一条单向的窄门：整包
/// `workspace.tar`。他看不见里面有什么，也送不进去任何东西 —— 手上那份 CSV
/// 只能贴成附件，而附件是给模型读的，不是工作区里的文件，agent 拿 `read_file`
/// 打不开它。
///
/// 三条端点补上了这两件事，这个文件是它们在界面上的落点。
///
/// # 为什么懒加载
///
/// 与桌面端那棵本机树（`workspace_panel.dart`）同一个理由，只是这里更贵：
/// 每展开一层是**一次网络往返**。急切递归的话，一个装着 `node_modules` 的
/// 工作区会打出上千个请求，而用户第一眼要看的就是顶层那几个文件。
library;

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../api/cortex_api.dart';
import '../../core/local_agent.dart';
import '../../core/save_file.dart';
import '../../models/attachment.dart' show formatBytes;
import '../../models/workspace.dart';
import '../../state/app_providers.dart';
import 'sandbox_download_button.dart';

/// 挂在 `WorkspacePanel` 的 Web 那一支：一句说明 + 文件树 + 两个出入口。
///
/// 平台判据只在这里出现一次，而且判的是**能力**（这个构建有没有自己的
/// agent）而不是 `kIsWeb`。桌面端 agent 动的就是用户自己的目录，那儿的
/// 「工作区」是本机文件树，云沙箱在那儿是个看不见你项目的更弱选项。
///
/// 判据留在外层而不是塞进 [SandboxBrowser]：那样这棵树在桌面上的测试里
/// 会整个变成 `SizedBox.shrink`，于是「懒加载」「501 原样透出」这些
/// 断言在 CI 上一条都跑不到。
class SandboxWorkspaceView extends StatelessWidget {
  const SandboxWorkspaceView({super.key});

  @override
  Widget build(BuildContext context) {
    if (kLocalAgentSupported) return const SizedBox.shrink();

    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(14, 10, 14, 8),
          child: Text(
            '浏览器读不到你这台机器的磁盘。下面是云端容器里的 '
            '$kSandboxRoot —— agent 读写的就是它，跨会话保留。',
            style: theme.textTheme.labelSmall,
          ),
        ),
        const Expanded(child: SandboxBrowser()),
      ],
    );
  }
}

/// 树本身。不做任何平台判断，因此在哪儿都能被 widget 测试驱动。
class SandboxBrowser extends ConsumerStatefulWidget {
  const SandboxBrowser({super.key, this.root = kSandboxRoot});

  /// 树根。可配置只是为了测试能盯着一个短路径，产品里恒为 [kSandboxRoot]。
  final String root;

  @override
  ConsumerState<SandboxBrowser> createState() => _SandboxBrowserState();
}

class _SandboxBrowserState extends ConsumerState<SandboxBrowser> {
  /// 已展开目录的绝对路径。按路径而不是下标存，兄弟节点增删后仍然对得上。
  final Set<String> _open = {};

  /// 列过至少一次的目录 → 它那一层的内容。**这就是懒加载的全部状态**：
  /// 键不在里面就是「还没要过」。
  final Map<String, List<FileNode>> _children = {};
  final Set<String> _loading = {};
  final Map<String, _Failure> _errors = {};

  /// 上传的落点。跟着用户点开的目录走，按钮上写着它的名字 ——
  /// 「传进去了但不知道传到哪儿」是这个入口最容易给出的坏体验。
  late String _target = widget.root;

  String? _downloading;
  bool _uploading = false;

  @override
  void initState() {
    super.initState();
    _open.add(widget.root);
    _load(widget.root);
  }

  /// [force] 为真时无视缓存重拉 —— 上传之后必须这样，否则新文件要等用户
  /// 手动折叠再展开才看得见。
  Future<void> _load(String path, {bool force = false}) async {
    if (_loading.contains(path)) return;
    if (!force && _children.containsKey(path)) return;
    setState(() {
      _loading.add(path);
      _errors.remove(path);
    });
    try {
      final nodes = await ref.read(cortexApiProvider).sandboxListFiles(path);
      if (!mounted) return;
      setState(() {
        _loading.remove(path);
        _children[path] = nodes;
      });
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        _loading.remove(path);
        _errors[path] = _Failure.from(e);
      });
    }
  }

  void _toggle(FileNode node) {
    setState(() {
      _target = node.path; // 点开哪个目录，就往哪个目录传
      if (!_open.remove(node.path)) _open.add(node.path);
    });
    if (_open.contains(node.path)) _load(node.path);
  }

  Future<void> _download(FileNode node) async {
    setState(() => _downloading = node.path);
    try {
      final bytes = await ref
          .read(cortexApiProvider)
          .sandboxReadFile(node.path);
      if (!mounted) return;
      await saveBytesAs(bytes, node.name);
    } on Object catch (e) {
      _say(_Failure.from(e).message);
    } finally {
      if (mounted) setState(() => _downloading = null);
    }
  }

  Future<void> _upload() async {
    final picked = await FilePicker.pickFiles(
      allowMultiple: true,
      // 桌面端默认只给路径，而这些字节要发出去；Web 上本来就是默认值
      withData: true,
      dialogTitle: '选择要传进工作区的文件',
    );
    if (picked == null || !mounted) return;

    setState(() => _uploading = true);
    final api = ref.read(cortexApiProvider);
    final target = _target;
    var done = 0;
    String? failure;
    for (final file in picked.files) {
      final bytes = file.bytes;
      if (bytes == null) continue;
      try {
        // `basenameOf` 挡掉选择器偶尔带出来的目录前缀。真正的越界围栏在
        // 服务端 —— 客户端这一道只是别把明显不对的东西发出去
        await api.sandboxWriteFile(
          path: posixJoin(target, basenameOf(file.name)),
          bytes: bytes,
        );
        done++;
      } on Object catch (e) {
        // 停在第一个失败上：后面那些多半会同样失败（容器没了、盘满了），
        // 而一串同样的红字只是把那一句真正的原因埋掉
        failure = _Failure.from(e).message;
        break;
      }
    }
    if (!mounted) return;
    setState(() => _uploading = false);

    // 成功了几个就刷新几个 —— 哪怕后面失败了，已经进去的那些也该出现在树上
    if (done > 0) {
      setState(() => _open.add(target));
      await _load(target, force: true);
    }
    if (!mounted) return;
    _say(
      failure ??
          (done == 0 ? '没有可上传的文件' : '已传入 $done 个文件到 ${basenameOf(target)}/'),
    );
  }

  void _say(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(child: _body(context)),
        _Actions(
          targetName: basenameOf(_target),
          busy: _uploading,
          onUpload: _upload,
        ),
      ],
    );
  }

  Widget _body(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final rootFailure = _errors[widget.root];
    if (rootFailure != null) {
      return _RootFailure(
        failure: rootFailure,
        onRetry: () => _load(widget.root, force: true),
      );
    }

    final rootChildren = _children[widget.root];
    if (rootChildren == null) {
      return const Center(
        child: SizedBox(
          width: 15,
          height: 15,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    // 与桌面端那棵树一样压平成一条列表，好让**一个** `ListView.builder`
    // 把整棵树虚拟化。嵌套 Column 会把每个展开节点的所有后代都 build 出来，
    // 而那正是懒加载想省下的那笔开销。
    final rows = <_Row>[];
    void walk(String path, int depth) {
      for (final node in _children[path] ?? const <FileNode>[]) {
        rows.add(_Row(node: node, depth: depth));
        if (!node.isDirectory || !_open.contains(node.path)) continue;
        if (_loading.contains(node.path)) {
          rows.add(_Row(node: node, depth: depth + 1, spinner: true));
        } else if (_errors[node.path] case final failure?) {
          rows.add(
            _Row(node: node, depth: depth + 1, message: failure.message),
          );
        } else {
          walk(node.path, depth + 1);
        }
      }
    }

    walk(widget.root, 0);

    if (rows.isEmpty) {
      return Center(child: Text('工作区还是空的', style: theme.textTheme.labelSmall));
    }

    return Scrollbar(
      child: ListView.builder(
        padding: const EdgeInsets.only(bottom: 4),
        itemCount: rows.length,
        itemExtent: 24,
        itemBuilder: (context, i) {
          final row = rows[i];
          if (row.spinner) {
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
          if (row.message case final message?) {
            return Padding(
              padding: EdgeInsets.only(left: 14.0 + row.depth * 13, right: 8),
              child: Text(
                message,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.error,
                ),
              ),
            );
          }
          return _NodeRow(
            node: row.node,
            depth: row.depth,
            expanded: _open.contains(row.node.path),
            isTarget: row.node.isDirectory && row.node.path == _target,
            busy: _downloading == row.node.path,
            onTap: () =>
                row.node.isDirectory ? _toggle(row.node) : _download(row.node),
          );
        },
      ),
    );
  }
}

/// 一次失败的两个面：给用户看的那句话，以及「这是不是容器不在」。
///
/// 服务端对容器被回收有一句专门的话（文件还在卷里，先发条消息把它拉起来）。
/// [message] **原样**存着它，界面绝不在前面拼「加载失败：」—— 那两件事在
/// 用户那儿的下一步完全不同：一个是发条消息，一个是去查后端。
class _Failure {
  const _Failure(this.message, {this.containerGone = false});

  factory _Failure.from(Object error) {
    if (error is CortexApiException) {
      // 409 而不是 501。两者服务端都用过：501 是「这个部署没开云沙箱」
      // （永久，重试没意义），409 是「容器被回收了」（发条消息就回来）。
      // 只看 501 的话，沙箱关掉的部署上会显示「沙箱容器不在了 + 重试」——
      // 标题与正文互相矛盾，而那个重试按钮永远按不出结果。
      return _Failure(error.message, containerGone: error.statusCode == 409);
    }
    return _Failure('$error');
  }

  final String message;
  final bool containerGone;
}

class _RootFailure extends StatelessWidget {
  const _RootFailure({required this.failure, required this.onRetry});

  final _Failure failure;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    // 容器不在**不是错误**：文件好好地待在卷里，用户发条消息就回来了。
    // 所以它拿的是中性色和一个「云」的图标，而不是红色的感叹号
    final color = failure.containerGone
        ? scheme.onSurfaceVariant
        : scheme.error;

    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                failure.containerGone
                    ? Icons.cloud_off_rounded
                    : Icons.error_outline_rounded,
                size: 15,
                color: color,
              ),
              const SizedBox(width: 7),
              Expanded(
                child: Text(
                  failure.containerGone ? '沙箱容器不在了' : '列目录失败',
                  style: theme.textTheme.labelMedium?.copyWith(color: color),
                ),
              ),
            ],
          ),
          const SizedBox(height: 6),
          // 服务端那句话，一字不改
          Text(failure.message, style: theme.textTheme.labelSmall),
          const SizedBox(height: 10),
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: onRetry,
              icon: const Icon(Icons.refresh_rounded, size: 15),
              label: const Text('重试'),
            ),
          ),
        ],
      ),
    );
  }
}

class _Actions extends StatelessWidget {
  const _Actions({
    required this.targetName,
    required this.busy,
    required this.onUpload,
  });

  final String targetName;
  final bool busy;
  final VoidCallback onUpload;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 6, 12, 10),
      // Wrap 而不是 Row：侧栏可以被拖窄，两个按钮并排放不下时换行而不是溢出
      child: Wrap(
        spacing: 8,
        runSpacing: 6,
        children: [
          OutlinedButton.icon(
            onPressed: busy ? null : onUpload,
            icon: busy
                ? const SizedBox(
                    width: 13,
                    height: 13,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.upload_file_rounded, size: 15),
            // 落点写在按钮上：用户点之前就该知道文件会去哪儿
            label: Text(busy ? '上传中…' : '传文件到 $targetName/'),
          ),
          const SandboxDownloadButton(),
        ],
      ),
    );
  }
}

class _Row {
  const _Row({
    required this.node,
    required this.depth,
    this.spinner = false,
    this.message,
  });

  final FileNode node;
  final int depth;
  final bool spinner;

  /// 这一层列不出来时就地显示的原因，同样是原文。
  final String? message;
}

class _NodeRow extends StatelessWidget {
  const _NodeRow({
    required this.node,
    required this.depth,
    required this.expanded,
    required this.isTarget,
    required this.busy,
    required this.onTap,
  });

  final FileNode node;
  final int depth;
  final bool expanded;
  final bool isTarget;
  final bool busy;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return InkWell(
      onTap: busy ? null : onTap,
      child: Container(
        padding: EdgeInsets.only(left: 10.0 + depth * 13, right: 8),
        color: isTarget ? scheme.secondary.withValues(alpha: 0.10) : null,
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
            if (busy)
              SizedBox(
                width: 13,
                height: 13,
                child: CircularProgressIndicator(
                  strokeWidth: 1.5,
                  color: scheme.onSurfaceVariant,
                ),
              )
            else
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
              child: Tooltip(
                message: node.isDirectory
                    ? '${node.path}/'
                    : '点击下载 ${node.path}',
                waitDuration: const Duration(milliseconds: 600),
                child: Text(
                  node.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.onSurface.withValues(alpha: 0.9),
                  ),
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
