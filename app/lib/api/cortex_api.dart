import '../core/permission_mode.dart';
import 'dart:typed_data';

import 'api_exception.dart';
import '../import/import_source.dart';
import '../models/account.dart';
import '../models/auth_tokens.dart';
import '../models/llm_key_status.dart';
import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/import_plan.dart';
import '../models/memory_search_result.dart';
import '../models/pending_confirmation.dart';
import '../models/project.dart';
import '../models/session_detail.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';
import '../models/workspace.dart';

/// Reports upload progress. [total] is 0 when the length is unknown.
typedef UploadProgress = void Function(int sent, int total);

/// 云沙箱工作区的根。容器里的这个路径挂着按用户持久的卷 —— 容器被回收后
/// 文件还在，这也是「容器不在」必须与「文件没了」分开说的原因。
const String kSandboxRoot = '/workspace';

/// `PUT /sandbox/files` 的回执。
///
/// 回的是服务端**规范化之后**的路径，不是请求里那个：后续要拿它去列目录或
/// 下载，用请求里那份拼出来的路径服务端可能根本没见过。
typedef SandboxWriteReceipt = ({String path, int size});

/// A short-lived credential that is allowed to travel in a URL.
class AuthTicket {
  const AuthTicket({required this.value, required this.expiresAt});

  final String value;

  /// Local wall-clock expiry, computed from the server's `expires_in_secs` at
  /// the moment of receipt — same reasoning as
  /// [PendingConfirmation.deadline]: a duration needs no agreement between
  /// clocks, a timestamp does.
  final DateTime expiresAt;

  /// True with a margin, because a ticket that expires mid-handshake produces a
  /// 401 the reconnect loop then has to interpret. Sixty seconds is short
  /// enough that spending some of it on safety costs nothing — the ticket is
  /// re-minted with one cheap POST.
  bool isUsableAt(DateTime now) =>
      expiresAt.difference(now) > const Duration(seconds: 10);
}

/// The whole surface the UI is allowed to touch.
///
/// Two implementations exist — [HttpCortexApi] (real `cortexd`) and
/// [MockCortexApi] (in-memory fixtures). Which one is live is decided once, in
/// `cortexApiProvider`; no widget or controller knows the difference.
abstract interface class CortexApi {
  /// Human-readable name of the active backend, shown in the status strip.
  String get label;

  /// `GET /health` — the **only** unauthenticated route.
  ///
  /// That is what makes it usable as the login gate's probe: it answers
  /// `auth: "token" | "disabled"` to a client that has no credential yet, which
  /// is the one question such a client needs answered before it can decide
  /// whether to demand one from the user.
  Future<HealthStatus> health();

  /// `POST /auth/ticket` — trade the long-lived token for a 60-second one.
  ///
  /// Exists because `WebSocket` (and `EventSource`, and `<img src>`) cannot
  /// carry an `Authorization` header in a browser — a hard API limitation, not
  /// something to engineer around. The ticket, and only the ticket, is allowed
  /// into a URL: query strings reach access logs, reverse-proxy logs and
  /// browser history, and a credential that expires in a minute survives that
  /// far better than one that never expires.
  ///
  /// Authenticated itself, so it doubles as the cheapest possible "is this
  /// token accepted?" probe — see `AuthController.signIn`.
  Future<AuthTicket> issueTicket();

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
  Future<bool> answerConfirmation({required String token, required bool allow});

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
  });

  /// `GET /memory/search?q=&limit=&as_of=`
  ///
  /// [asOf] replays the memory as it was *known* at that instant (transaction
  /// time), not as it was *true* — this is the "what did I believe three
  /// months ago" axis of the bitemporal model. Null means "now".
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  });

  /// `GET /episodes/{id}`
  Future<Episode> episode(String id);

  /// `GET /sessions?include_archived=&project_id=`
  ///
  /// Archived sessions are omitted by default. Archiving is not deletion —
  /// episodes, attachments and extracted memory are all untouched — so the
  /// toggle is the only thing standing between the user and their history.
  ///
  /// [projectId] 省略 = 全部会话（含未分组的）。侧边栏走的正是这一条：
  /// 它要一次画出所有分组，按项目拉 N 次是 N 倍的往返，而且**拉不到未分组
  /// 的那一组** —— 那一组没有 id 可以传。
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  });

  /// `GET /projects` —— 全部项目。
  ///
  /// 老服务端没有这条路由，答 404。调用方必须把它当作「这个部署没有项目
  /// 功能」优雅降级（[CortexApiException.isUnsupported]），而不是让整个
  /// 侧边栏变成一块错误提示 —— 会话列表本身与项目毫无关系，它照样能用。
  Future<List<Project>> projects();

  /// `POST /projects {"name": …}`
  Future<Project> createProject(String name);

  /// `PATCH /projects/{id} {"name": …}`
  Future<Project> renameProject(String id, String name);

  /// `DELETE /projects/{id}` —— **只删这层分组**。
  ///
  /// 里面的会话、消息、附件、抽出来的记忆一概不动，它们变成未分组。
  /// 界面上的确认文案必须把这一条说清楚：用户在这里最怕的事情是
  /// 「删项目把对话也删了」，而那是这个调用唯一**不会**发生的事。
  Future<void> deleteProject(String id);

  /// `PATCH /sessions/{id} {"project_id": …}` —— 移入 / 移出项目。
  ///
  /// [projectId] 为 null 时发的是显式的 `null`（移出，变未分组），
  /// 与 [updateSession] 里 workspace 的三态是同一套道理：
  /// 「字段不在」与「字段是 null」必须表达两件不同的事。
  Future<ChatSession> moveSessionToProject(String sessionId, String? projectId);

  /// `GET /sessions/{id}?limit=&before=` — overview plus **one page** of
  /// episodes, oldest first within the page.
  ///
  /// With no [before] the page is the *newest* one. To walk backwards, pass the
  /// previous response's `nextCursor`; stop when `hasMore` is false. [limit] is
  /// clamped server-side to 500.
  ///
  /// The cursor is opaque — constructing one locally earns a 400, which is the
  /// point: it is validated before it reaches SQL.
  Future<SessionDetail> sessionDetail(String id, {int? limit, String? before});

  /// `POST /auth/login` —— 用户名 + 密码换一对令牌。
  ///
  /// 拿回来的 `refreshToken` 是**「下次打开不用再登录」的全部依据**，
  /// 调用方必须把它存进平台的凭据库（见 `app/lib/auth/`）。
  Future<AuthTokens> login(String username, String password);

  /// `POST /auth/refresh` —— 用长效凭据换一对新的。
  ///
  /// 每次都会**轮转**：服务端把旧的作废并签一个新的。所以调用方必须
  /// 存下新的那个 —— 继续用旧的会被判成重放，而重放会让整条链一起失效。
  Future<AuthTokens> refreshSession(String refreshToken);

  /// `GET /auth/me` —— 当前这把凭据对应的是谁。
  ///
  /// 返回 null 有两种意思，**对调用方是同一种**（没有名字可显示）：
  /// 这个后端没有这个端点（老服务端），或者压根没有账号体系
  /// （mock / 关掉认证的部署）。两种都不该让账号栏消失 ——
  /// 它还挂着设置与退出登录。
  Future<Account?> whoAmI();

  /// `POST /auth/logout` —— 让服务端作废这条链。
  ///
  /// 与「本地删掉副本」不是一回事：本地删除只让这台机器忘了，
  /// 而已经泄露出去的那一份照样能用到 30 天后。
  Future<void> logout(String refreshToken);

  /// `GET /settings/llm-key` —— 填没填过自带 key、填的是哪一把。
  Future<LlmKeyStatus> llmKeyStatus();

  /// `PUT /settings/llm-key` —— 存一把自己的 key。
  ///
  /// 明文只在这一次请求体里出现，之后再也拿不回来（服务端只回后 4 位）。
  Future<LlmKeyStatus> setLlmKey({
    required String provider,
    required String apiKey,
    String? baseUrl,
  });

  /// `DELETE /settings/llm-key` —— 撤下，回到用服务端那把（重新占配额）。
  Future<LlmKeyStatus> clearLlmKey();

  /// Hands the daemon a file to parse, and gets back something cheap to refer
  /// to it by.
  ///
  /// Desktop passes a path straight through — the local agent opens it itself,
  /// so nothing moves. Web uploads via `POST /import/upload` and gets a spool
  /// handle. **This is the only place the 97 MB travels**; preview and run both
  /// take the handle, so the file is sent once rather than once per step.
  Future<ImportTarget> prepareImport(ImportSource source);

  /// The bill. Reads the file, writes nothing.
  ///
  /// A separate endpoint from [runImport] rather than a `dryRun: true` flag, so
  /// that calling the wrong one cannot cost money: a read-only endpoint has no
  /// path to a write. Every pair in the estimate is one LLM call, and memory is
  /// append-only — undoing an import means `redact`, which is deliberately
  /// explicit and confirmed twice.
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  });

  /// Does it. Emits [ImportStartedEvent], then [ImportProgressEvent]s, then
  /// exactly one [ImportDoneEvent] or [ImportErrorEvent].
  ///
  /// Safe to rerun: episode ids are derived from (original timestamp, stable
  /// seed) and the daemon dedupes on them, so a second run only fills gaps and
  /// re-bills nothing.
  Stream<ImportEvent> runImport(ImportTarget target, {int? maxConversations});

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
  Future<String> createLocalWorkspace({required String name, String? projectId});

  /// `POST /local/workspaces/{id}/auto` — 按日期时间开一个文件夹并绑上。
  ///
  /// 给「新建会话时没选工作区」那一档用。时间戳的格式在 agent 那一侧 ——
  /// 客户端有两个（这个和 CLI），写两遍就会漂成两种。
  Future<String?> autoBindLocalWorkspace(String id);

  /// `PATCH /sessions/{id}` — rename, archive, bind a workspace.
  ///
  /// The three fields are independent and all optional; only what is passed is
  /// changed, in one server-side write transaction.
  ///
  /// Workspace is tri-state and the two "no path" cases mean opposite things:
  ///
  /// | call | wire | meaning |
  /// |---|---|---|
  /// | neither arg | field absent | leave the binding alone |
  /// | `clearWorkspace: true` | `"workspace": null` | unbind, back to plain chat |
  /// | `workspace: "D:/x"` | `"workspace": "D:/x"` | bind |
  ///
  /// The daemon validates the path (absolute, exists, is a directory, not a
  /// filesystem root / system directory / the home directory itself, checked
  /// after symlink resolution) and its rejection message is written to be shown
  /// to the user verbatim — so callers should surface it, not replace it.
  ///
  /// Throws with [CortexApiException.isUnsupported] against a daemon that
  /// predates the route; callers keep the change local and flag it unsynced.
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  });

  /// `POST /blobs` — server-relayed upload, for content up to
  /// `kRelayUploadLimit`.
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/presign` — ask for a direct-to-object-store URL.
  ///
  /// Throws with `isUnsupported == true` on deployments backed by the local
  /// filesystem, which cannot sign URLs.
  Future<BlobPresign> presignBlob(String hash);

  /// `PUT` straight to the object store using a presigned URL. Not a cortexd
  /// endpoint — the whole point is that these bytes never touch the daemon.
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/commit` — register a directly-uploaded object.
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  });

  /// `GET /blobs/{hash}` — the bytes, relayed by the daemon.
  ///
  /// Deliberately bytes rather than a URL: the mock source has no origin to
  /// hand out, and going through one method keeps the attachment widgets from
  /// having to know which backend they are on.
  Future<Uint8List> blobBytes(String hash);

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

  /// `GET /ws` — **one** connection attempt.
  ///
  /// The returned stream ends when the socket closes and errors when it cannot
  /// be opened. Reconnection is deliberately *not* handled here: backoff,
  /// attempt counting and the cursor are policy, and policy belongs to
  /// `SyncController` where it can be unit-tested against a fake socket.
  Stream<SyncEvent> watchSync();

  /// `GET /sync?since=&limit=`
  ///
  /// [since] must be the caller's **own** cursor. Passing a cursor taken from a
  /// [SyncEvent] would skip everything between what we have and what the server
  /// reached — see the contract note on [SyncEvent].
  Future<SyncPage> sync({required int since, int limit = 500});

  void dispose();
}

/// 「这个实现不支持自带 key」。
///
/// mock、回放替身、以及测试里那几个假 API 都是这样 —— 它们没有可以存
/// 密钥的地方。写成 mixin 而不是让每个类各写三个方法：这个接口每加一个
/// 方法，五个测试替身就要跟着补一遍，而漏掉的那个直到编译才发现。
///
/// **如实回 `supported: false`**，不假装支持：假装的结果是设置页给出一个
/// 存不进去的输入框，用户填了、点保存、看起来成功了、下次打开又是空的。
/// **不要用在真实客户端上**：那样删掉 `HttpCortexApi` 里任何一个方法都不会
/// 编译报错，而是静默退化成「这个部署不支持」—— 用户看到的是入口消失了。
/// 同上，给 [CortexApi.whoAmI] 用。
///
/// 单开一个而不是并进 [LlmKeyUnsupported]：那个名字说的是「存不了 API key」，
/// 把「答不出我是谁」塞进去，下一个读到 `with LlmKeyUnsupported` 的人会
/// 以为这个替身只是没有密钥存储。名字与它承诺的事对不上，是这一类 mixin
/// 最容易积累的债。
///
/// 与那边同一条禁令：**不要用在真实客户端上**。
mixin AccountUnsupported {
  Future<Account?> whoAmI() async => null;
}

/// 同上，给四条本地工作空间路由用。
///
/// 抛 404 而不是回一个空值：调用方对 404 已经有正确的处理（当成「这台机器上
/// 没有本机工作空间」），而回 `LocalWorkspaceRoot.empty` 会让界面显示一个
/// 「根目录：无」的设置项，看起来像功能坏了。
///
/// 与那边同一条禁令：**不要用在真实客户端上**。
mixin LocalWorkspaceUnsupported {
  static const _absent = CortexApiException(
    '这个后端没有本地工作空间。',
    statusCode: 404,
  );

  Future<LocalWorkspaceRoot> localWorkspaceRoot() async => throw _absent;

  Future<LocalWorkspaceRoot> setLocalWorkspaceRoot(String path) async =>
      throw _absent;

  Future<String> createLocalWorkspace({
    required String name,
    String? projectId,
  }) async => throw _absent;

  Future<String?> autoBindLocalWorkspace(String id) async => throw _absent;
}

mixin LlmKeyUnsupported {
  Future<LlmKeyStatus> llmKeyStatus() async => LlmKeyStatus.empty;

  Future<LlmKeyStatus> setLlmKey({
    required String provider,
    required String apiKey,
    String? baseUrl,
  }) async => LlmKeyStatus.empty;

  Future<LlmKeyStatus> clearLlmKey() async => LlmKeyStatus.empty;
}
