import '../models/workspace.dart';

/// A browser tab cannot enumerate the user's disk.
const bool kCanBrowseLocalFiles = false;

/// Shown verbatim wherever the tree or the picker would have been.
///
/// It explains the *product* reason rather than the technical one, because the
/// technical one ("no `dart:io` on Web") answers a question the user did not
/// ask. The thing worth saying is that even a perfect browser file API would
/// not help: the agent runs inside cortexd, so the path it needs is a path on
/// **cortexd's** machine, which the browser has never seen.
const String kNoLocalFilesReason =
    'Web 端跑在浏览器沙箱里，读不到本机目录；而且工作区是 cortexd 那台机器上的'
    '路径，即使浏览器能选目录，选出来的也是错的机器。所以这里改成直接填写'
    'cortexd 可见的绝对路径 —— 路径由 daemon 校验，填错会当场告诉你哪里错了。';

/// Always null on Web: there is no native directory dialog to open.
///
/// Callers must gate on [kCanBrowseLocalFiles] and offer the path-entry flow
/// instead. Returning null unconditionally is the safety net, not the design —
/// a button that silently does nothing is exactly what this must not become.
Future<String?> pickWorkspaceDirectory() async => null;

/// Cannot be checked from a browser.
///
/// Returns true rather than false so a bound session is not misreported as
/// broken: the daemon already validated this path at bind time, and it is the
/// daemon's filesystem that matters. On Web the client simply has no
/// independent opinion, and inventing a negative one would be worse than
/// having none.
Future<bool> workspaceExists(String path) async => true;

Future<List<FileNode>> listWorkspaceDirectory(String path) async =>
    throw UnsupportedError(kNoLocalFilesReason);
