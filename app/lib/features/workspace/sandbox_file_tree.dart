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
import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../api/cortex_api.dart';
import '../../core/formatting.dart';
import '../../core/save_file.dart';
import '../../models/attachment.dart' show formatBytes;
import '../../models/workspace.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import 'file_preview.dart';
import 'sandbox_download_button.dart';

/// 挂在 `WorkspacePanel` 的**云端**那一支：一句说明 + 文件树 + 两个出入口。
///
/// # 这里**不再**做平台判断
///
/// 它从前第一行就是 `if (kLocalAgentSupported) return SizedBox.shrink()`，
/// 也就是「桌面端一律不画」。那个判断当时对（桌面端的会话都绑本机目录），
/// 但它按**构建**判 —— 于是桌面端打开一个**云端**会话时，文件在容器的卷里，
/// 而这块面板要么显示「你还没绑目录」，要么（判据修好之后）显示一片空白。
///
/// 现在「这个会话的文件在哪」只由 `WorkspacePanel` 判一次，见它那段注释。
/// 判据留在一处的理由与从前一样：**两处判据迟早说不到一块去**。
class SandboxWorkspaceView extends StatelessWidget {
  const SandboxWorkspaceView({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(14, 10, 14, 8),
          child: Text(
            // 不提「容器」「沙箱」。用户要知道的只有两件事：
            // 这些文件不是他本机的，以及 agent 动的就是它们。
            //
            // 措辞不能再从「浏览器」说起：桌面端上的云端会话也走这一支，
            // 而那儿的用户明明有磁盘 —— 说「浏览器读不到你的磁盘」是句错话
            '这个会话跑在远端，agent 有一份自己的工作区 —— 不是你本机的目录。'
            '下面就是它，跨会话保留。',
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

  /// 正在预览哪个文件。null = 预览面板不出现。
  FileNode? _preview;

  /// 预览的内容。与 [_preview] 分开存，因为「选中了但还在读」是一个真实状态，
  /// 而合成一个字段会让它与「读完了但是空文件」分不开。
  Uint8List? _previewBytes;
  String? _previewError;
  bool _previewLoading = false;

  /// 当前在看哪个会话。**每一次文件请求都要带上它。**
  ///
  /// 服务端拿它查这个会话属于哪个项目，再决定读写哪个卷
  /// （`SandboxScope::key`）。漏传的后果不是报错，是**打开的是另一个
  /// 工作区** —— 未分组那个，通常是空的。而界面上看起来一切正常。
  String? get _session => ref.read(chatControllerProvider).activeSession?.id;

  /// 上传进行到哪一个。`null` = 没在传。
  ///
  /// 只做**逐文件**进度，不做逐字节：Web 上 `package:http` 的 `BrowserClient`
  /// 根本不暴露上传进度（fetch/XHR 的流式上传它没接出来），做出来只能在桌面端
  /// 生效 —— 而一个「有时候有进度条」的界面比没有更让人困惑。
  ///
  /// 逐文件已经足够消掉「界面只是卡着」：用户看得见在传第几个、传的是哪个。
  SandboxUpload? _upload_;

  /// 「一轮结束了」的订阅。见 [initState]。
  ProviderSubscription<bool>? _turnEnd;

  @override
  void initState() {
    super.initState();
    _open.add(widget.root);
    _load(widget.root);
    // ── 一轮跑完就自动重列 ──
    //
    // 用 `listenManual` 而不是在 `build` 里 `ref.listen`：这棵树的 build 会
    // 因为展开/折叠反复跑，而订阅只需要建一次。
    //
    // 判据是「流从有到没有」。**不用工具事件**：那样得先认出哪些工具会写文件
    // （`write_file`、`shell`、外来 MCP 工具……），而漏认一个就是「这次没刷新」，
    // 一条静默的漏。一轮结束多列一次目录，代价是每个展开层一个请求。
    _turnEnd = ref.listenManual(
      chatControllerProvider.select((s) => s.streaming != null),
      (was, now) {
        if (was == true && now == false) unawaited(_refresh());
      },
    );
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
      final nodes = await ref
          .read(cortexApiProvider)
          .sandboxListFiles(path, sessionId: _session);
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

  /// 把已经展开的那几层**重新列一遍**，并顺带重读正在预览的那个文件。
  ///
  /// # 为什么必须有这个
  ///
  /// 这棵树列过一层就记住（`_children` 就是懒加载的全部状态），而最常见的
  /// 那件事恰恰是「agent 刚写完一个文件，我看看」—— 打开树，里面没有它。
  /// 真机上撞到过：agent 写完 `demo.md`，树上没有，切会话也没用，
  /// **只有把整栏关掉再打开**才重列（那时部件被销毁，缓存跟着没了）。
  ///
  /// # 只重列展开的那几层
  ///
  /// 折叠着的目录用户看不见，为它们发请求等于把懒加载又退回急切加载 ——
  /// 而那正是这棵树最初要避开的东西（一个带 `node_modules` 的工作区
  /// 会打出上千个请求）。
  Future<void> _refresh() async {
    // 先拍一份快照：`_load` 会 `setState`，而在迭代 `_open` 时改它是错的
    final open = _open.toList();
    await Future.wait([for (final path in open) _load(path, force: true)]);
    // 预览也可能过期了：agent 刚重写的就是它。文件没了就让它显示那个错误 ——
    // 比继续展示一份已经不存在的内容诚实
    if (_preview case final node?) await _openPreview(node);
  }

  @override
  void dispose() {
    // `listenManual` 的订阅不跟着部件走，得自己关。不关的后果是这个 State
    // 被销毁之后回调还在，而它第一件事就是 `setState`
    _turnEnd?.close();
    super.dispose();
  }

  void _toggle(FileNode node) {
    setState(() {
      _target = node.path; // 点开哪个目录，就往哪个目录传
      if (!_open.remove(node.path)) _open.add(node.path);
    });
    if (_open.contains(node.path)) _load(node.path);
  }

  /// 点一个文件 = 看它，而不是下载它。
  ///
  /// 从前点一下直接触发下载。那对「把产物拿走」是对的，对更常见的
  /// 「agent 说它写了 report.md，写了什么」则是绕远路 —— 下载、找下载目录、
  /// 用别的程序打开，只为看三行字。下载挪到了预览面板的标题栏上。
  ///
  /// 太大的文件**先不读**：判据用目录列表里的 `size`，于是一个 200 MB 的日志
  /// 连请求都不会发出去。读回来再判的话，那 200 MB 已经过了网络、进了内存
  /// （Web 上是浏览器的堆）。
  Future<void> _openPreview(FileNode node) async {
    final tooBig = (node.sizeBytes ?? 0) > kPreviewMaxBytes;
    setState(() {
      _preview = node;
      _previewBytes = null;
      _previewError = null;
      _previewLoading = !tooBig;
    });
    if (tooBig) return;
    try {
      final bytes = await ref
          .read(cortexApiProvider)
          .sandboxReadFile(node.path, sessionId: _session);
      if (!mounted) return;
      // 读的过程中用户可能又点了别的文件 —— 那时这份结果已经过期。
      // 不判的话，慢的那次请求回来会把快的那次覆盖掉（经典的乱序落地）
      if (_preview?.path != node.path) return;
      setState(() {
        _previewBytes = bytes;
        _previewLoading = false;
      });
    } on Object catch (e) {
      if (!mounted || _preview?.path != node.path) return;
      setState(() {
        _previewError = _Failure.from(e).message;
        _previewLoading = false;
      });
    }
  }

  Future<void> _download(FileNode node) async {
    setState(() => _downloading = node.path);
    try {
      final bytes = await ref
          .read(cortexApiProvider)
          .sandboxReadFile(node.path, sessionId: _session);
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

    final api = ref.read(cortexApiProvider);
    final target = _target;
    // 循环开始前取一次：中途用户切了会话的话，剩下的文件也该落在
    // 他点「上传」时看的那个工作区里，而不是分散到两个卷
    final session = _session;
    final queue = picked.files.where((f) => f.bytes != null).toList();
    var done = 0;
    String? failure;
    for (final file in queue) {
      final bytes = file.bytes;
      if (bytes == null) continue;
      final name = basenameOf(file.name);
      // 每一个都刷一次：这一行就是「不是卡死了」的全部证据
      setState(
        () => _upload_ = SandboxUpload(
          done: done,
          total: queue.length,
          name: name,
          bytes: bytes.length,
        ),
      );
      try {
        // `basenameOf` 挡掉选择器偶尔带出来的目录前缀。真正的越界围栏在
        // 服务端 —— 客户端这一道只是别把明显不对的东西发出去
        await api.sandboxWriteFile(
          path: posixJoin(target, name),
          bytes: bytes,
          sessionId: session,
          onProgress: (sent, total) {
            if (!mounted) return;
            setState(
              () => _upload_ = SandboxUpload(
                done: done,
                total: queue.length,
                name: name,
                sent: sent,
                bytes: total,
              ),
            );
          },
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
    setState(() => _upload_ = null);

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
    final preview = _preview;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(child: _body(context)),
        // 预览占下半栏，**上限是一半**：再高就把文件树挤没了，而用户还得
        // 靠那棵树切到下一个文件。`Flexible` + `maxHeight` 让短文件只占它
        // 实际需要的高度，长文件到一半为止再滚
        if (preview != null)
          ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(context).height * 0.5,
            ),
            child: FilePreview(
              node: preview,
              bytes: _previewBytes,
              error: _previewError,
              loading: _previewLoading,
              onDownload: () => _download(preview),
              onClose: () => setState(() {
                _preview = null;
                _previewBytes = null;
                _previewError = null;
              }),
            ),
          ),
        _Actions(
          targetName: basenameOf(_target),
          progress: _upload_,
          onUpload: _upload,
          onRefresh: _refresh,
          refreshing: _loading.isNotEmpty,
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
            onTap: () => row.node.isDirectory
                ? _toggle(row.node)
                : _openPreview(row.node),
          );
        },
      ),
    );
  }
}

/// 一次失败：给用户看的那句话。
///
/// [message] **原样**存着服务端说的，界面绝不在前面拼「加载失败：」——
/// 服务端已经把话说完整了，再包一层只会把它挤成一行灰字。
///
/// # 这里曾经有一个 `containerGone`
///
/// 容器被回收时服务端回 409，界面显示「沙箱容器不在了 / 拉起来了，刷新」。
/// 那条路今天不存在了：列目录自己就会把容器拉起来（服务端的
/// `ensure_for_files`）。用户不该知道有个容器，更不该被要求去把它弄回来。
class _Failure {
  const _Failure(this.message);

  factory _Failure.from(Object error) =>
      _Failure(error is CortexApiException ? error.message : '$error');

  final String message;
}

class _RootFailure extends StatelessWidget {
  const _RootFailure({required this.failure, required this.onRetry});

  final _Failure failure;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.error_outline_rounded, size: 15, color: scheme.error),
              const SizedBox(width: 7),
              Expanded(
                child: Text(
                  '打不开文件',
                  style: theme.textTheme.labelMedium?.copyWith(
                    color: scheme.error,
                  ),
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

/// 上传进行到哪一个。逐文件，不逐字节 —— 理由见 `_SandboxBrowserState._upload_`。
///
/// 公开只为了测 [label]：整条上传链路要过 `FilePicker` 的平台通道，
/// widget 测试驱不动；而这里唯一有判断的就是那一句文案。
@visibleForTesting
class SandboxUpload {
  const SandboxUpload({
    required this.done,
    required this.total,
    required this.name,
    this.sent = 0,
    this.bytes = 0,
  });

  /// 已经**传完**的个数。当前这个不算在内，所以显示时要 +1。
  final int done;
  final int total;

  /// 正在传的那个文件名。只传一个文件时它比「1/1」有用得多 ——
  /// 用户想知道的是「卡在哪个文件上」。
  final String name;

  /// 当前这个文件已交出去的字节数 / 它的总字节数。
  ///
  /// **Web 上这个数会先冲到 100% 然后停在那儿等。** 浏览器不支持流式请求体
  /// （要 HTTP/2，Firefox 与 Safari 都没有），所以 `FetchClient` 是先把整个流
  /// 抽干再上传 —— 详见 `http_cortex_api.dart` 的 `_ProgressRequest`。
  /// 所以满了之后显示「收尾中」而不是一个撒谎的 100%。
  final int sent;
  final int bytes;

  /// 只在**大到值得报**的时候才报百分比。
  ///
  /// 64 KiB 正是分块大小：比它小的文件只会有一次回调，闪一下百分比
  /// 只是噪声。
  String? get _pct {
    if (bytes < 64 * 1024) return null;
    return sent >= bytes ? '收尾中' : '${(sent * 100 / bytes).floor()}%';
  }

  /// 单个文件时只报名字：「1/1」是一句废话。
  String get label {
    final head = total == 1 ? '正在传 $name' : '正在传 ${done + 1}/$total：$name';
    final pct = _pct;
    return pct == null ? head : '$head $pct';
  }
}

class _Actions extends StatelessWidget {
  const _Actions({
    required this.targetName,
    required this.progress,
    required this.onUpload,
    required this.onRefresh,
    required this.refreshing,
  });

  final String targetName;
  final SandboxUpload? progress;
  final VoidCallback onUpload;

  /// 手动重列。**自动刷新之外还要留这一个**：一轮结束会自动重列，但文件也可能
  /// 由别的东西改（另一台设备上的同一个会话、同项目的另一个会话、
  /// 用户自己刚传进来的那一批）—— 那几种情况没有「一轮结束」这个信号。
  final Future<void> Function() onRefresh;

  final bool refreshing;

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
            onPressed: progress == null ? onUpload : null,
            icon: progress != null
                ? const SizedBox(
                    width: 13,
                    height: 13,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.upload_file_rounded, size: 15),
            // 落点写在按钮上：用户点之前就该知道文件会去哪儿。
            // 传的时候换成进度 —— 「上传中…」在传十个文件时与卡死没有区别
            label: Text(progress?.label ?? '传文件到 $targetName/'),
          ),
          const SandboxDownloadButton(),
          // 图标按钮而不是带文字的：这一排已经有两个长按钮，侧栏窄的时候
          // 第三个会把它们挤到第三行
          IconButton(
            onPressed: refreshing ? null : () => unawaited(onRefresh()),
            iconSize: 16,
            tooltip: '重新列一遍',
            icon: refreshing
                ? const SizedBox(
                    width: 13,
                    height: 13,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.refresh_rounded),
          ),
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
            // 修改时间在前、大小在后。它回答的是「这是 agent 刚写的那个吗」，
            // 而那是用户扫这棵树时最常问的问题 —— 比大小更值钱。
            //
            // 目录也显示：一个「刚刚」的目录说明这一轮往里面写过东西。
            // 服务端给不出时（tar 里没这个字段）就整个不画，**不画 1970 年**。
            if (node.modifiedAt != null)
              Padding(
                padding: const EdgeInsets.only(right: 6),
                child: Text(
                  formatRelative(node.modifiedAt),
                  style: theme.textTheme.labelSmall?.copyWith(
                    fontSize: 9,
                    color: scheme.onSurfaceVariant.withValues(alpha: 0.75),
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
