/// A directory the agent is allowed to read and write for a given session.
///
/// ## Whose filesystem is this?
///
/// **cortexd's.** The file tools (`read_file` / `write_file` / `list_dir`) run
/// inside the daemon, fenced by `cortex_agent::Sandbox`. A workspace is
/// therefore an absolute path *on the machine running cortexd* — not on the
/// machine running this UI. On a desktop install those are the same machine, so
/// a native directory picker produces a usable path. On Web they are usually
/// not, which is why the Web binding flow asks for a path instead of opening a
/// picker (see `features/workspace/workspace_binding_sheet.dart`).
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

/// One entry in the workspace file tree.
class FileNode {
  const FileNode({
    required this.name,
    required this.path,
    required this.isDirectory,
    this.sizeBytes,
  });

  final String name;

  /// Absolute path, so a node can be expanded without re-deriving it from its
  /// ancestors.
  final String path;
  final bool isDirectory;
  final int? sizeBytes;
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
