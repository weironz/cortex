import '../models/workspace.dart';

/// A browser tab cannot enumerate the user's disk.
const bool kCanBrowseLocalFiles = false;

/// Shown verbatim wherever the tree or the picker would have been.
///
/// It explains the *product* reason rather than the technical one, because the
/// technical one ("no `dart:io` on Web") answers a question the user did not
/// ask.
///
/// # 这段话改过一次，原因值得记下来
///
/// 原文说的是「填一个 cortexd 那台机器上的绝对路径」。任务 #75 把文件与
/// shell 工具从 cortexd 卸掉之后，那条路就被服务端 400 拒了 —— 而这段话
/// 还在教用户怎么走它。**指着一条已经不存在的路，比什么都不说更糟**：
/// 用户会以为是自己路径填错了，反复试。
///
/// 现在的真实形状是两条路，都写在这里：本机 agent（桌面端）或云沙箱。
const String kNoLocalFilesReason =
    'Web 端跑在浏览器沙箱里，读不到本机目录 —— 而且服务端进程自己也不再执行'
    '文件与命令，填一个服务器上的路径会被拒。要让 agent 读写文件有两条路：'
    '用桌面端（agent 跑在你自己的机器上，动的就是这些目录），'
    '或者在输入框打开「云沙箱」（工作区是云端容器里的 /workspace，跨会话保留）。';

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
