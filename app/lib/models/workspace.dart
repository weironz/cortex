/// A directory the agent is allowed to read and write for a given session.
///
/// ## Whose filesystem is this?
///
/// **Whichever side runs the turn** — and that is what `session.runtime`
/// records. On desktop the local agent runs it, so this is a path on *this*
/// machine and a native directory picker produces a usable value. On Web the
/// turn runs in a cloud container, so this is a path inside that container
/// (`/workspace`), never a path the browser's machine could reach.
///
/// The two never mix: cortexd refuses a turn for a session pinned to a local
/// runtime rather than silently running it against the server's disk.
class Workspace {
  const Workspace({required this.root, this.label});

  /// Absolute path on the daemon's filesystem.
  final String root;

  /// Display name. Defaults to the last path segment.
  final String? label;

  String get displayName {
    if (label != null && label!.isNotEmpty) return label!;
    return basenameOf(root);
  }

  Workspace copyWith({String? label}) =>
      Workspace(root: root, label: label ?? this.label);

  @override
  bool operator ==(Object other) =>
      other is Workspace && other.root == root && other.label == label;

  @override
  int get hashCode => Object.hash(root, label);
}

/// 默认工作空间根目录，以及它下面已有的文件夹。
///
/// 这是**设备本地设置**，权威在本地 agent（`GET /local/workspace-root`）——
/// 真正建目录、校验路径的是它，而同机 CLI 与桌面端共用同一个进程，所以
/// 把它放进客户端的偏好设置会让两边各有一份。
class LocalWorkspaceRoot {
  const LocalWorkspaceRoot({required this.root, required this.folders});

  /// **可能还不存在**：用户没设过时这是建议值（`~/Cortex`），磁盘上要到
  /// 第一次真用到才落地。界面照常显示它 —— 那正是它要回答的问题
  /// 「我的文件会去哪儿」。
  final String? root;

  /// 根目录下已有的文件夹名（不含隐藏目录），用作可选工作空间。
  final List<String> folders;

  static const empty = LocalWorkspaceRoot(root: null, folders: []);
}

/// One entry in the workspace file tree.
class FileNode {
  const FileNode({
    required this.name,
    required this.path,
    required this.isDirectory,
    this.sizeBytes,
    this.modifiedAt,
  });

  final String name;

  /// Absolute path, so a node can be expanded without re-deriving it from its
  /// ancestors.
  final String path;
  final bool isDirectory;
  final int? sizeBytes;

  /// 最后修改时间。**可空，而且空是常态** —— 桌面端那棵本机树目前不填它。
  ///
  /// 它回答的是「这是 agent 刚写的那个吗」：一个几十个文件的工作区里，
  /// 光看名字和大小分不出哪些是这一轮的产物。
  ///
  /// 用 `null` 而不是 epoch 表示未知：0 会被渲染成 1970-01-01，
  /// 一个煞有介事的假日期比一个空格误导得多。
  final DateTime? modifiedAt;
}

/// Last path segment, tolerant of both separators (a Windows daemon path can be
/// typed into a Web client running on macOS and vice versa).
String basenameOf(String path) {
  var p = path;
  while (p.length > 1 && (p.endsWith('/') || p.endsWith('\\'))) {
    p = p.substring(0, p.length - 1);
  }
  final i = p.lastIndexOf(RegExp(r'[/\\]'));
  if (i < 0 || i == p.length - 1) return p;
  return p.substring(i + 1);
}

/// Joins a directory and a single entry name with `/`.
///
/// 只用于云沙箱那一侧的路径 —— 容器里是 Linux，分隔符恒为 `/`，不该跟着
/// **客户端**所在的平台走。用 `dart:io` 的 `path.join` 会在 Windows 上拼出
/// `\`，而那条路径是要发回给容器的。
String posixJoin(String dir, String name) =>
    dir.endsWith('/') ? '$dir$name' : '$dir/$name';

/// Makes [path] relative to [root] for display, keeping forward slashes.
///
/// Falls back to the input when it is not under [root] — which happens whenever
/// the daemon is on another machine and the path came from somewhere else.
String relativeTo(String root, String path) {
  final r = root.replaceAll('\\', '/').replaceAll(RegExp(r'/+$'), '');
  final p = path.replaceAll('\\', '/');
  if (r.isNotEmpty && p.startsWith('$r/')) return p.substring(r.length + 1);
  return p;
}
