part of '../cortex_api.dart';

mixin CortexApiChat {
  /// `GET /confirmations?session_id=` — what is still waiting for an answer.
  ///
  /// This is the **reconnect** path. `POST /chat`'s SSE stream is single-shot
  /// with no `Last-Event-ID` replay, so a confirmation request that was in
  /// flight when the socket dropped is never re-sent; without this call the
  /// turn would sit suspended until it timed out, with nothing on screen to
  /// explain why. It is also how a *second* device discovers that the first one
  /// was asked something.
  Future<List<PendingConfirmation>> pendingConfirmations({String? sessionId});

  /// `POST /confirmations` — deliver the user's answer.
  ///
  /// Returns false when the daemon answers 404, which is **not** a failure: the
  /// token is one-shot and four ordinary situations consume it — another device
  /// answered first, the 180-second timeout fired, the turn ended, or the
  /// daemon restarted. The server refuses to distinguish them (telling them
  /// apart would let someone probe for "is a command being approved right
  /// now?"), so neither can the client. Callers should retire the prompt
  /// quietly, not raise an error.
  ///
  /// [allow] false sends `deny`, which is a *different* thing from letting the
  /// clock run out even though the agent treats both as refusal: a denial
  /// arrives immediately and frees the suspended turn, rather than parking a
  /// database connection and a model context for three minutes.
  /// 回答一条确认。
  ///
  /// [sessionId] **必须带上**：云端那条路要靠它找到这一轮跑在哪个沙箱容器里，
  /// 而容器是按项目分的。不带的话回执会落到「未分组」那个沙箱上 —— 服务端
  /// 会诚实地回 409（不会跑去别人容器里翻），但用户点的那一下就此石沉大海。
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  });

  /// `POST /chat` → SSE.
  ///
  /// The returned stream is single-subscription and must be cancelled by the
  /// caller to abort an in-flight generation.
  ///
  /// [attachments] must already be **registered** blobs (via [uploadBlob] or
  /// presign+[commitBlob]); this call only associates them, it never carries
  /// bytes.
  ///
  /// There is deliberately no workspace parameter here. The sandbox root is a
  /// property of the *session*, set once through [updateSession]; sending it
  /// per message would put the fence's key on the same channel as the message
  /// the fence exists to contain.
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments,
    PermissionMode permissionMode,

    /// 这一轮用哪个模型。`null` = 部署配的那个，`'auto'` = 自动档。
    ///
    /// 逐轮带，与 [permissionMode] 同一路数：用户在设置里换了模型，
    /// **下一句**就该按新的走。
    String? model,
    String? source,

    /// 这一轮用哪个智能体的人设。`null` = 默认那个（通用助理）。
    ///
    /// 逐轮带，与 [model] 完全同构。⚠️ 它在一条会话里应当**保持稳定**：
    /// 系统提示词是可缓存前缀的第一段，逐轮换人设等于每一轮都在打穿
    /// prompt caching。所以界面上「换智能体」= 开一条新对话。
    Assistant? assistant,

    /// 这一轮**有哪些技能可用** —— 只有名字与说明，没有正文。
    ///
    /// 服务端据此在系统提示词里渲染一小块目录，并且**只有在这个列表非空时**
    /// 才把 `load_skill` 摆进工具目录。正文由那个工具按需取回。
    ///
    /// ⚠️ 不带正文正是分层的全部意义：贵的那一半只在真要用时才进上下文。
    List<Skill> skills,

    /// 这一轮准不准操作电脑（截屏 + 键鼠）。
    ///
    /// ⚠️ **默认 false，而这个默认值是安全属性的一部分。** 这一组工具没有
    /// 围栏 —— 它动的是用户整台机器上正在运行的一切。
    ///
    /// 它只是「用户准了」：够不够得着还要看 agent 跑在哪（容器里没有屏幕），
    /// 那一半由服务端判。
    bool computerUse,

    /// 这一轮如果画图，按什么规格。`null` = 完全听模型的。
    ///
    /// 与 [permissionMode] 同一路数：逐轮带。图片页底下那个规格面板随时
    /// 能改，改完**下一句**就该按新的走。
    ImagePrefs? imagePrefs,
  });

  /// `GET /runs/{session_id}` → SSE —— 挂上一个**已经在跑**的轮次。
  ///
  /// # 这条路存在的理由
  ///
  /// 轮次跑在服务端一个独立的 task 里，客户端断开不会中止它。缺的一直是
  /// 回来的路：关掉标签页再打开，那一轮还在干活，而界面上什么也看不到，
  /// 只能等 episode 落库。
  ///
  /// 拿到的流与 [chat] **完全同形**（重放 + 后续拼成一条），所以调用方
  /// 那套事件处理一行都不用改。
  ///
  /// # 抛 404 是正常路径
  ///
  /// 绝大多数会话此刻都没在跑。调用方应当把 `statusCode == 404` 当成
  /// 「没在跑」而不是故障 —— 与 `localWorkspaceRoot` 那条同一个路数。
  Stream<ChatEvent> attachChat(String sessionId);

  /// `DELETE /runs/{session_id}` —— 真的把服务端那一轮掐掉。
  ///
  /// # 为什么客户端自己取消订阅不算「停止」
  ///
  /// 取消订阅只是**不看了**：服务端那一轮照跑，继续烧 token、继续按模型的
  /// 意思改文件，而屏幕上写着「已停止生成」。一个说了假话的按钮比没有更糟。
  ///
  /// 404 不是错误：那一轮可能刚好自己结束了。调用方按「已经停了」处理。
  Future<void> stopRun(String sessionId);
}
