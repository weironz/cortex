import 'dart:typed_data';

import '../models/workspace.dart';

/// A browser tab cannot enumerate the user's disk.
const bool kCanBrowseLocalFiles = false;

/// Shown verbatim wherever the tree or the picker would have been.
///
/// It explains the *product* reason rather than the technical one, because the
/// technical one ("no `dart:io` on Web") answers a question the user did not
/// ask.
///
/// # 这段话改过**两次**，两次都是同一个形状
///
/// 第一版说「填一个 cortexd 那台机器上的绝对路径」。任务 #75 把文件与
/// shell 工具从 cortexd 卸掉之后，那条路被服务端 400 拒了 —— 而这段话
/// 还在教用户怎么走它。
///
/// 第二版改成「在输入框打开『云沙箱』开关」。那个开关后来也删了
/// （见 `ChatRequest` 的注释：它把实现细节推给了用户）—— 于是这段话
/// **又**指向了一个不存在的控件。
///
/// **指着一条已经不存在的路，比什么都不说更糟**：用户会以为是自己没找到，
/// 反复找。教训是「别在文案里点名一个控件」—— 控件会被删，而删的人
/// 不会想到来搜这里。现在这段只描述**能力**，不描述怎么点。
const String kNoLocalFilesReason =
    'Web 端跑在浏览器沙箱里，读不到本机目录 —— 而且服务端进程自己也不再执行'
    '文件与命令，填一个服务器上的路径会被拒。但这不代表 agent 不能读写文件：'
    '在 Web 端它有一份自己的云端工作区（跨会话保留，就在左下角那个「文件」里），'
    '不需要你做任何事。要让它动**你这台机器上**的目录，用桌面端。';

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

/// Web 上没有本机文件可读。**这条永远不会被调到**：没有本地 agent 的构建
/// 压根不会构造本机文件树（判据在 `WorkspacePanel`）。留着是为了这道缝的
/// 两半暴露同一组名字 —— 少一个名字，条件导入会在编译期就红。
Future<Uint8List> readWorkspaceFile(String path) =>
    throw UnsupportedError(kNoLocalFilesReason);
