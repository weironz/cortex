part of '../cortex_api.dart';

mixin CortexApiSandbox {
  /// `GET /sandbox/workspace.tar` — 把云沙箱的整个工作区打包取回来。
  ///
  /// 没有这条的话，agent 在容器卷里写出来的东西**用户永远拿不到**：
  /// 工具行上写着「写了 report.md」，然后就没有然后了。
  ///
  /// 容器已被回收时服务端回 4xx 并说清「文件还在卷里，先发条消息把它拉起来」
  /// —— 那不是数据丢了，而两者在界面上必须分得开。
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId});

  /// `GET /sandbox/files?path=` —— 云沙箱工作区的**一层**目录。
  ///
  /// 一层而不是整棵树：`node_modules` / `target` 走一遍要好几秒，而用户第一眼
  /// 要看的就是顶层。哪一层被展开了再要哪一层 —— 与桌面端那棵本机文件树
  /// （`workspace/workspace_fs.dart`）是同一个形状，出于同一个理由。
  ///
  /// [path] 必须是 [kSandboxRoot] 之内的绝对路径。越界由**服务端**判（400）：
  /// 客户端再抄一份检查，两份迟早会不一致，而不一致的方向通常是
  /// 「客户端以为安全」。
  ///
  /// 容器已被回收时回 **409**（不是 501 —— 那个留给「这个部署没开云沙箱」，
  /// 客户端据它把功能永久降级）。`message` 是服务端专门写给用户看的那句话
  /// （文件还在卷里，先发条消息把它拉起来）。**调用方必须原样透出** ——
  /// 「容器不在」与「加载失败」在用户那儿的下一步完全不同：前者发条消息就好，
  /// 后者要去查网络或后端。
  ///
  /// 返回的 [FileNode.path] 是绝对路径，因此一个节点不必回溯祖先就能展开。
  Future<List<FileNode>> sandboxListFiles(String path, {String? sessionId});

  /// `GET /sandbox/files/raw?path=` —— 取一个文件的原始字节。
  ///
  /// 与 [sandboxWorkspaceTar] 并存而不是取代它：整包是「把这次的产物全拿走」，
  /// 这条是「就要那一个 report.md」。让用户为了一个文件下载整个卷，
  /// 与让他为了看一眼目录先解一次 tar，是同一种不体面。
  Future<Uint8List> sandboxReadFile(String path, {String? sessionId});

  /// `PUT /sandbox/files?path=` —— 把字节写进云沙箱工作区。
  ///
  /// 这是**用户往容器里送文件的唯一入口**。没有它，Web 端的 agent 只能处理
  /// 它自己生成的东西：用户手上那份 CSV 永远进不去 —— 附件走的是 blob 存储，
  /// 那是给模型读的，不是工作区里的文件。
  ///
  /// [path] 含文件名，服务端按同样的围栏判越界。同名覆盖由服务端决定，
  /// 客户端不先探测再写：探测与写之间那一小段时间里 agent 也在写同一个卷。
  Future<SandboxWriteReceipt> sandboxWriteFile({
    String? sessionId,
    required String path,
    required Uint8List bytes,
    UploadProgress? onProgress,
  });
}
