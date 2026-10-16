part of '../cortex_api.dart';

mixin CortexApiWorkspace {
  /// `GET /agents` —— 我名下**此刻在线**的机器。
  ///
  /// 离线的不在列表里（服务端只回 TTL 内的），所以这个列表**每次都要现拉**：
  /// presence 刻意不进库、也不进 `sync_log`（心跳每 30 秒一行写入会挤满
  /// 同步流水并下发给所有设备），于是它没有「变了会通知你」这条路。
  ///
  /// `session` 给了的话，每行多一个 `hasSession` —— 「这个会话在哪台机器上」
  /// 就是靠它答的。
  Future<List<AgentPresence>> agents({String? session});

  /// `GET /local/attach` —— **这台机器**接不接受远程接入。
  ///
  /// 只有本机 agent 答得出：Web 端与纯 cortexd 会 404/405，
  /// 调用方据此**整个不画那个开关**（做不到就别摆出来，与电脑操作那节
  /// 同一条纪律）。
  Future<LocalAttach> localAttach();

  /// `PUT /local/attach` —— 拨动它，回落定之后的状态。
  ///
  /// ⚠️ 打开它 = 同意**远程侧可经模型在这台机器上执行命令与读写文件**：
  /// 接入面里 `POST /chat` 与 `POST /confirmations` 并存，接进来的一方
  /// 能发起一轮并自己批准工具确认。界面文案不许把这句写软成
  /// 「允许远程查看」（安全不变量 4）。
  Future<LocalAttach> setLocalAttach(bool enabled);

  /// `PUT /local/workspaces/{id}` — bind or unbind on **this device**.
  ///
  /// Only the local agent answers this. It never touches the network, which is
  /// the whole point: the path is on this machine, so binding must work with
  /// the daemon unreachable. Going through `PATCH /sessions/{id}` instead had
  /// two symptoms, both observed:
  ///
  /// - offline: the binding hit `workspaces.json`, then the forward 502'd and
  ///   the user was told it failed;
  /// - online: the daemon answered with `workspace: null` (the local agent
  ///   nulls it on the way out, by design) and the UI went straight back to
  ///   "unbound".
  ///
  /// Returns the **canonicalised** path, or `null` after an unbind. Throws
  /// [CortexApiException] with `isUnsupported` when talking to a plain cortexd
  /// or a local agent older than this route — callers fall back to
  /// [updateSession].
  Future<String?> bindLocalWorkspace(String id, String? path);

  /// `GET /local/workspace-root` — 默认工作空间根目录 + 它下面已有的文件夹。
  ///
  /// 只有本地 agent 答得了：那个目录在这台机器上。对着一个纯 cortexd
  /// （Web 端）调会抛 `isUnsupported`，调用方按「没有本机」处理。
  Future<LocalWorkspaceRoot> localWorkspaceRoot();

  /// `PUT /local/workspace-root` — 改默认根目录。只校验、不搬迁已有数据。
  Future<LocalWorkspaceRoot> setLocalWorkspaceRoot(String path);

  /// `POST /local/workspaces` — 在根目录下开一个工作空间目录，回它的绝对路径。
  ///
  /// 带 [projectId] 就顺便记账，于是**项目改名之后不会再建第二个文件夹**。
  /// 建目录与绑会话刻意分成两步：绑定那条路上还有清放行清单、同步执行归属
  /// 这些必须发生的事，不该复制一份到这里。
  Future<String> createLocalWorkspace({
    required String name,
    String? projectId,
  });

  /// `POST /local/workspaces/{id}/auto` — 按日期时间开一个文件夹并绑上。
  ///
  /// 给「新建会话时没选工作区」那一档用。时间戳的格式在 agent 那一侧 ——
  /// 客户端有两个（这个和 CLI），写两遍就会漂成两种。
  Future<String?> autoBindLocalWorkspace(String id);

  // ── MCP ────────────────────────────────────────────
  //
  // 这一族与上面的工作空间同类：**设备本地**。配置文件在这台机器上、
  // 子进程也在这台机器上跑，cortexd 两样都没有 —— 所以 Web 端会拿到 404，
  // 调用方按「这个后端没有本机 MCP」处理。
}
