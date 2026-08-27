import '../core/permission_mode.dart';
import 'dart:typed_data';

import 'api_exception.dart';
import '../import/import_source.dart';
import '../models/account.dart';
import '../models/auth_tokens.dart';
import '../models/library_item.dart';
import '../models/model_source.dart';
import '../models/search_prefs.dart';
import '../models/model_role.dart';
import '../models/mcp.dart';
import '../models/agent_presence.dart';
import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/generated_image.dart';
import '../models/image_prefs.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/import_plan.dart';
import '../models/pending_confirmation.dart';
import '../models/assistant.dart';
import '../models/project.dart';
import '../models/skill.dart';
import '../models/session_detail.dart';
import '../models/model_option.dart';
import '../models/session_search_hit.dart';
import '../models/usage_report.dart';
import '../models/sandbox_health.dart';
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

  /// `GET /sandbox/health` —— 「这个部署跑不跑得了**云端对话**」。
  ///
  /// 与 [health] 分成两次请求，因为生产上它们**根本不是同一个进程**：
  /// 边缘按路径分流，`/health` 归记忆服务、这一条才是 agent 编排服务。
  /// 为什么 `/health` 通了还不够，见 [SandboxHealth] 的文档。
  ///
  /// 也是免认证的公开路由（消费者是配不了凭据的探针），所以登录之前
  /// 就能问 —— 与 [health] 同一个理由。
  ///
  /// # **不抛异常**，每一种失败都是一个要显示的答案
  ///
  /// 404、一张 SPA 回落的网页、网关的 502 —— 这些在别处是故障，在这里
  /// 分别是「这个部署没有沙箱」与「有但现在跑不起来」，都是要画到界面上
  /// 的能力说明。让它抛的话，每个调用方都得自己判一次「这个错是不是其实
  /// 正常」，而判漏的那一处会把「自托管没有沙箱」画成一个红框，
  /// 让用户去修一个没坏的东西。与 `localWorkspaceRootProvider` 同一立场。
  Future<SandboxHealth> sandboxHealth();

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

  /// `GET /sessions/search?q=` —— 在标题与消息正文里找。
  ///
  /// 搜索在**服务端**做：全部消息只在那张库里，客户端手上只有当前会话
  /// 这一页，拿它去搜等于「只能搜到刚才看过的东西」。
  ///
  /// 老服务端没有这条路由，答 404 —— 调用方按
  /// [CortexApiException.isUnsupported] 优雅降级成「这个部署搜不了」，
  /// 而不是把侧边栏整块换成错误提示。
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  });

  /// `GET /llm/models` —— 这个部署能用哪些模型。
  ///
  /// # 为什么必须先拿这份列表，而不是让用户填一个名字
  ///
  /// 填错的表现是**每一轮对话都失败**，而错误来自供应商（「no such
  /// model」），看不出是选错了。有了列表，选择才做得成一个选不错的下拉框。
  ///
  /// 老服务端没有这条路由，答 404 —— 那时模型选择器整个不出现
  /// （[CortexApiException.isUnsupported]），而不是给一个点下去报错的框。
  Future<ModelCatalog> llmModels();

  /// `GET /auth/usage` —— 这个窗口用了多少、花了多少、还剩多少。
  ///
  /// 服务端一直在记这笔账（`cortex_auth.usage`，每次 LLM 调用一行），
  /// 但在这条路接上之前**没有任何地方看得见它** —— 用户唯一会知道
  /// 自己用了多少的时刻，是撞上配额被 429 拦下那一次。
  ///
  /// 没接账号体系的部署（自托管单人）没有这条路，答 404 ——
  /// 那时用量这一页整个不出现，见 [CortexApiException.isUnsupported]。
  Future<UsageReport> usage();

  /// `GET /projects` —— 全部项目。
  ///
  /// 老服务端没有这条路由，答 404。调用方必须把它当作「这个部署没有项目
  /// 功能」优雅降级（[CortexApiException.isUnsupported]），而不是让整个
  /// 侧边栏变成一块错误提示 —— 会话列表本身与项目毫无关系，它照样能用。
  Future<List<Project>> projects();

  /// `GET /assistants` —— 全部智能体。
  ///
  /// ⚠️ **路由叫 assistants 不叫 agents**：服务端那条 `/agents` 是
  /// 「哪些本机 agent 进程在线」，两个 agent 不是一回事。
  ///
  /// 老服务端答 404，调用方按 [CortexApiException.isUnsupported] 优雅降级。
  Future<List<Assistant>> assistants();

  /// `POST /assistants`
  Future<Assistant> createAssistant(Assistant draft);

  /// `PATCH /assistants/{id}` —— 只发真的要改的那几个字段。
  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,

    /// 整份替换（不是增量）。`null` = 这次不改它。
    List<String>? disabledTools,
  });

  /// `DELETE /assistants/{id}`
  ///
  /// **不影响已经用它聊过的会话**：人设是逐轮带的，历史一个字都不会变。
  Future<void> deleteAssistant(String id);

  /// `GET /skills` —— 全部技能（**含正文**，设置页要编辑它）。
  ///
  /// ⚠️ 发给 `/chat` 的那一份**不含正文**（`Skill.toBrief()`）——
  /// 分层的意义就在那一点。
  ///
  /// 老服务端答 404，调用方按 [CortexApiException.isUnsupported] 优雅降级。
  Future<List<Skill>> skills();

  /// `POST /skills`
  ///
  /// 重名会被拒（名字在服务端带 UNIQUE：模型用它取正文，重名会取错）。
  Future<Skill> createSkill(Skill draft);

  /// `PATCH /skills/{id}` —— 只发真的要改的那几个字段。
  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  });

  /// `DELETE /skills/{id}`
  ///
  /// **不影响已经聊过的会话**：目录是逐轮带的、正文是当场取回来的，
  /// 两样都已经落进历史里的消息了。
  Future<void> deleteSkill(String id);

  /// `POST /projects {"name": …}`
  Future<Project> createProject(String name);

  /// `PATCH /projects/{id}` —— 改名和/或置顶。
  ///
  /// 两者是**两台独立的状态机**（服务端各写一条事件），所以两个参数各自
  /// 可空：只给 `pinned` 就只改置顶，名字一个字都不动。
  Future<Project> patchProject(String id, {String? name, bool? pinned});

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

  /// `PATCH /sessions/{id} {"container_workspace": …}` —— 换云沙箱里的子目录。
  ///
  /// [name] 为 null 时发的是**显式的** `null`，意思是「回到卷根」；
  /// 与 [moveSessionToProject] 是同一套三态约定（「字段不在」= 别动）。
  ///
  /// 这是**云端**那一支的「选工作区」。桌面端那一支走
  /// [bindLocalWorkspace]，两者不共用一条路：本机路径是设备本地概念，
  /// 而这个名字是服务端记着的会话状态。
  ///
  /// 服务端校验名字的形状（`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`，与库里那条
  /// CHECK 是同一份规则），拒绝时的措辞是写给用户看的 —— 原样显示，别替换。
  Future<ChatSession> setContainerWorkspace(String sessionId, String? name);

  /// `DELETE /runs/{session_id}` —— 真的把服务端那一轮掐掉。
  ///
  /// # 为什么客户端自己取消订阅不算「停止」
  ///
  /// 取消订阅只是**不看了**：服务端那一轮照跑，继续烧 token、继续按模型的
  /// 意思改文件，而屏幕上写着「已停止生成」。一个说了假话的按钮比没有更糟。
  ///
  /// 404 不是错误：那一轮可能刚好自己结束了。调用方按「已经停了」处理。
  Future<void> stopRun(String sessionId);

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

  /// `POST /auth/register` —— 开号并直接换回一对令牌（注册即登录）。
  ///
  /// 只在部署开着注册（`/health` 的 `open_registration`）时才该被调用 ——
  /// 关着的部署回 403，正文里写着管理员该怎么开。界面靠那个字段决定
  /// **摆不摆**入口；这条本身不做那个判断。
  Future<AuthTokens> register(String username, String password);

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
  Future<bool> localAttach();

  /// `PUT /local/attach` —— 拨动它，回落定之后的状态。
  ///
  /// ⚠️ 打开它 = 同意**远程侧可经模型在这台机器上执行命令与读写文件**：
  /// 接入面里 `POST /chat` 与 `POST /confirmations` 并存，接进来的一方
  /// 能发起一轮并自己批准工具确认。界面文案不许把这句写软成
  /// 「允许远程查看」（安全不变量 4）。
  Future<bool> setLocalAttach(bool enabled);

  /// `GET /settings/search` —— 联网检索的配置。
  ///
  /// 回的东西里**没有明文 key**（只有后四位），与模型来源同一条约定。
  Future<SearchPrefs> searchPrefs();

  /// `PATCH /settings/search` —— 改配置。
  ///
  /// ⚠️ **每一位都可缺省，缺省 = 不动。** 只想改「结果个数」的人不该被要求
  /// 重填一遍 key —— 而界面手上根本没有明文。`apiKey` 传空串 = **清掉**，
  /// 与「没提这一位」是两回事。
  Future<SearchPrefs> saveSearchPrefs({
    String? provider,
    String? apiKey,
    String? baseUrl,
    int? maxResults,
    String? depth,
    int? cutoffLimit,
    List<String>? excludeDomains,
  });

  /// `GET /settings/model-sources` —— 全部模型来源（含部署提供的那条）。
  Future<ModelSources> modelSources();

  /// 新增（`id == null`）或修改一条来源。
  ///
  /// 明文 key 只在这一次请求体里出现，之后再也拿不回来（服务端只回后 4 位）。
  /// 改一条时 `apiKey` 留空 = **不动原来那把**。
  Future<ModelSources> saveModelSource({
    String? id,
    required String provider,
    String apiKey,
    String label,
    String? baseUrl,
    bool? enabled,
    List<String>? models,
  });

  /// 删掉一条。部署提供的那条删不掉（服务端会拒）。
  Future<ModelSources> deleteModelSource(String id);

  /// 去问这条来源的供应商它到底有哪些型号。
  ///
  /// 拉不动时服务端回落到内置定义并把 `live` 置 false —— 界面要说出来。
  Future<FetchedModels> fetchSourceModels(String id);

  /// 手工按下某个模型的能力位。
  ///
  /// # 为什么需要人来按
  ///
  /// **OpenAI 的 `/v1/models` 一个能力字段都不返回**，而绝大多数
  /// OpenAI 兼容中转站照抄这个形状。也就是说自带网关的人，他那些模型
  /// 能不能看图、能不能调工具，没有任何自动来源说得出来。
  ///
  /// 传一份**完整的**覆盖记录（缺省的位 = 「这一位我没意见」）。
  /// 一位都没按时服务端会把整条删掉。
  Future<ModelSources> saveModelCaps(
    String sourceId,
    String modelId,
    CapsOverride caps,
  );

  /// 拿这条来源存下来的 key 真发一次请求，验「我填对了没有」。
  ///
  /// **不能在客户端做**：明文 key 从不下发（服务端只回后 4 位），
  /// 这里没有任何东西可以拿去试。
  ///
  /// [model] 留空 = 让服务端挑这条来源开着的第一个。
  Future<SourceCheck> checkModelSource(String id, {String model = ''});

  /// `GET /settings/model-roles` —— 现在把哪个模型指派给了哪个角色。
  Future<RoleAssignments> modelRoles();

  /// `PUT /settings/model-roles` —— **整份替换**。
  ///
  /// 服务端会校验（来源还在不在、那条来源开没开放这个型号、绘画角色指的
  /// 是不是真的画得出来），不合法直接 400 —— 所以界面要么筛掉不合法的
  /// 选项，要么把这个错误原样显示出来。
  Future<RoleAssignments> saveModelRoles(RoleAssignments roles);

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

  /// `GET /local/mcp` — 配置 + 逐台状态 + 每台的工具。
  Future<McpConfigView> mcpConfig();

  /// `PUT /local/mcp/servers/{name}` — 加一台或改一台，落盘后立刻重连。
  ///
  /// [config] 是原样的传输配置（`command`/`args`/`env` 或 `url`/`headers`）。
  /// **env 是合并的**：没给的保持原样，要删的列进 [removeEnv] —— 客户端手上
  /// 从来没有过旧的值（服务端只回名字），所以做不了替换。
  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  });

  /// `DELETE /local/mcp/servers/{name}`
  Future<McpConfigView> deleteMcpServer(String name);

  /// `POST /local/mcp/reload` — 重读文件、重新连。
  ///
  /// 用户手编过配置之后要有一条路能生效，否则「配置文件在这里」那句话
  /// 就是假的：告诉了位置，却要重启才算数。
  Future<McpConfigView> reloadMcp();

  /// `POST /local/mcp/parse` — 粘一段进来，看看会变成什么。**不落盘**。
  ///
  /// 与落盘分成两步是刻意的：加一台 MCP server = 在这台机器上跑任意进程，
  /// 中间必须有一屏让用户看到那条命令行原文。
  Future<List<McpParsedServer>> parseMcpPaste(String text);

  /// `GET /local/mcp/registry?q=` — 代查官方 MCP 注册表。
  Future<List<McpRegistryEntry>> searchMcpRegistry(String query);

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
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  });

  /// `POST /sessions/{id}/fork` — 分叉：带着历史开一条新会话，旧会话不动。
  ///
  /// [upToEpisodeId] 给了就截到那条消息（**含**它）——「从这里分叉」；
  /// 不给就整段复制。返回**新会话**，标题是「原标题（分叉）」。
  ///
  /// 复制发生在服务端一个写事务里（消息、工具轨迹、附件引用；附件字节
  /// 内容寻址，不复制）。客户端拿到返回值后直接切过去即可 —— 历史按
  /// 正常的 [sessionDetail] 拉取，不需要任何本地拼装。
  Future<ChatSession> forkSession(String id, {String? upToEpisodeId});

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

  /// `GET /blobs/{hash}/url` —— 一条**会过期**的直链，给「复制链接」用。
  ///
  /// 与 [blobBytes] 是两件事：那条是**我自己要看**（中转，永远能用），
  /// 这条是**发给别人**（直链，对象存储签的，十五分钟）。
  ///
  /// 部署用本地文件系统当对象存储时签不出来，服务端回 501 ——
  /// 界面据此把按钮置灰**并说明**，而不是让它点下去弹一句红字。
  Future<BlobUrl> blobUrl(String hash);

  /// `POST /llm/image` —— 画几张图，回它们的 blob 哈希。
  ///
  /// # 为什么只回哈希
  ///
  /// 图在服务端就抓下来入库了（供应商给的链接只活 24 小时）。取图与附件
  /// 走同一条路（[blobBytes]），所以这里不需要第二种「图片从哪来」。
  ///
  /// 提示词、型号、时间这些由画廊（[gallery]）回答 —— 画完刷一次即可，
  /// 一份数据一个来源。
  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  });

  /// `GET /images` —— 画廊，按时间倒序翻页。
  ///
  /// [folder] 只看某个文件夹里的（`"none"` = 只看未归档的）；
  /// [hash] 只看某份字节对应的那一行。
  ///
  /// [hash] 是给**对话里那张图**用的：附件只带哈希，而分享 / 移除要的是
  /// 画廊那一行的 id。没有它，同一张图在对话里右键出来的菜单会比图库里
  /// 少几项 —— 而用户根本分不清那是两个东西。
  Future<Gallery> gallery({
    int limit,
    String? before,
    String? folder,
    String? hash,
  });

  /// `POST /images/{id}/share` —— 拿一条**公开**链接（免登录、长期有效）。
  ///
  /// 重复调用回同一条。界面上必须说清它是公开的 —— 用户以为自己只是
  /// 「复制了个地址」，而那是把这张图放到了公网上。
  Future<String> shareImage(String id);

  /// `DELETE /images/{id}/share` —— 撤销。那条链接当场 404。
  Future<void> unshareImage(String id);

  /// `DELETE /images/{id}` —— 从图库移除。
  ///
  /// **blob 不动**：对话里那张图照常显示。所以界面上这个动作叫
  /// 「从图库移除」，不叫「删除图片」。
  Future<void> removeImage(String id);

  /// `GET /folders` —— 文件夹清单（图片与资料**共用**，带项数与封面）。
  Future<Folders> folders();

  /// `GET /library` —— 资料库的一页。
  ///
  /// [folder] 只看某个文件夹（`"none"` = 只看未归档的）；
  /// [tab] 是 `all` / `images` / `files`。
  Future<LibraryPage> library({
    int limit,
    String? before,
    String? folder,
    String? tab,
  });

  /// `POST /library` —— 把一份**已登记的 blob** 收进资料库。
  ///
  /// 字节要先走 `/blobs`（与附件同一条路）。同一份内容重复收会回原来
  /// 那条而不是报错 —— 拖两次同一个文件不是错误。
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  });

  /// `PATCH /library/{id}` —— 改名 / 移动到文件夹。
  ///
  /// [folderId] 显式传 `null` 且 [moveFolder] 为真 = 移出文件夹。
  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  });

  /// `DELETE /library/{id}` —— 从资料库移除。**blob 不动。**
  Future<void> removeFromLibrary(String id);

  /// `POST /folders` —— 新建。回**整份列表**，客户端不必自己拼。
  Future<Folders> createFolder(String name);

  /// `PATCH /folders/{id}` —— 改名。
  Future<Folders> renameFolder(String id, String name);

  /// `DELETE /folders/{id}` —— 删文件夹。**里面的东西一件都不会没**，
  /// 它们回到「未归档」（服务端靠 `ON DELETE SET NULL` 保证）。
  Future<Folders> deleteFolder(String id);

  /// `PATCH /images/{id}` —— 把一张图移进 / 移出文件夹。
  ///
  /// 传 `null` = 移出来。**归档是排他的**，所以「移进 A」自带「离开 B」——
  /// 不需要客户端拆成两次请求（拆了的话中间断网就是一张谁也不属于的图）。
  Future<void> moveImage(String id, String? folderId);

  /// `GET /sandbox/workspace.tar` — 把云沙箱的整个工作区打包取回来。
  ///
  /// 没有这条的话，agent 在容器卷里写出来的东西**用户永远拿不到**：
  /// 工具行上写着「写了 report.md」，然后就没有然后了。
  ///
  /// 容器已被回收时服务端回 4xx 并说清「文件还在卷里，先发条消息把它拉起来」
  /// —— 那不是数据丢了，而两者在界面上必须分得开。
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId});

  /// `GET /sessions/export` —— 把这个账号的全部会话导成一份 NDJSON。
  ///
  /// **导的是会话，不是记忆**：facts / 时间轴在 Cormex 那个服务里，
  /// 这一侧一列都没有。界面上必须说清这一点 —— 一个写着「导出我的数据」
  /// 的按钮如果只给一半，用户会以为另一半不存在。
  ///
  /// 附件只有 hash 与文件名，字节走 `/blobs/{hash}` —— 内联的话一份导出
  /// 可能有几个 GB。
  Future<Uint8List> exportSessions();

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
/// 单开一个而不是并进 [ModelSourcesUnsupported]：那个名字说的是「存不了 API key」，
/// 把「答不出我是谁」塞进去，下一个读到 `with ModelSourcesUnsupported` 的人会
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
  static const _absent = CortexApiException('这个后端没有本地工作空间。', statusCode: 404);

  Future<LocalWorkspaceRoot> localWorkspaceRoot() async => throw _absent;

  Future<LocalWorkspaceRoot> setLocalWorkspaceRoot(String path) async =>
      throw _absent;

  Future<String> createLocalWorkspace({
    required String name,
    String? projectId,
  }) async => throw _absent;

  Future<String?> autoBindLocalWorkspace(String id) async => throw _absent;
}

/// 同上，给 MCP 那几条用。
///
/// 与 [LocalWorkspaceUnsupported] 分开而不是并进去：那个名字说的是
/// 「没有本地工作空间」，把 MCP 塞进去之后，下一个读到 `with` 那一行的人
/// 会以为这个替身只是不能绑目录。名字与它承诺的事对不上，是这一类 mixin
/// 最容易积累的债。
///
/// 与那边同一条禁令：**不要用在真实客户端上**。
/// 给答不了 `GET /runs/{id}` 的后端用：**一律「没在跑」**。
///
/// 旧版本的 agent、以及每一个测试替身都属于这一类。抛 404 而不是回一个
/// 空流：调用方对 404 已经有正确的处理（当成没在跑），而一个立刻结束的
/// 空流会被当成「挂上了但那一轮瞬间结束」——于是界面上闪一下「正在生成」。
///
/// 与那边同一条禁令：**不要用在真实客户端上**。
/// 给测试替身用：这个后端搜不了。
///
/// 回**抛错**而不是空列表：空列表与「真的一条都没搜到」长得一模一样，
/// 于是一个忘了接搜索的后端在界面上表现为「搜索能用，只是永远没结果」。
/// 给测试替身用：这个后端没有模型列表。
mixin LlmModelsUnsupported {
  Future<ModelCatalog> llmModels() =>
      Future.error(const CortexApiException('这个后端没有模型列表。', statusCode: 404));
}

mixin SessionSearchUnsupported {
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  }) => Future.error(const CortexApiException('这个后端不支持搜索。', statusCode: 404));
}

/// 给测试替身用：这个后端答不出在线名册。
///
/// ⚠️ **不要用在真实客户端上**（与这一族其余的同一条禁令）：用了之后
/// `HttpCortexApi` 漏实现这个方法不会编译报错，而是静默退化成
/// 「一台在线的机器都没有」—— 而那与「机器真的都关着」长得一模一样，
/// 于是「我的机器」那一页永远空着，没人看得出是没实现。
///
/// 回**抛错**而不是空列表，正是为了让这个区别留在屏幕上。
mixin AgentsUnsupported {
  Future<List<AgentPresence>> agents({String? session}) =>
      Future.error(const CortexApiException('这个后端答不出在线名册。', statusCode: 404));
}

/// 给测试替身用：这个后端答不出远程接入开关（Web 端、纯 cortexd、老 agent）。
///
/// ⚠️ 与 [`AgentsUnsupported`] 同一条禁令，而且这一条错了更贵：静默退化成
/// 「关着」的话，一个**真的开着**远程接入的机器会在界面上显示成关着 ——
/// 用户以为自己没开，而云端接得进来。
mixin LocalAttachUnsupported {
  Future<bool> localAttach() =>
      Future.error(const CortexApiException('这个后端没有远程接入开关。', statusCode: 404));
  Future<bool> setLocalAttach(bool enabled) =>
      Future.error(const CortexApiException('这个后端没有远程接入开关。', statusCode: 404));
}

/// 给测试替身用：这个后端没有用量端点。
mixin UsageUnsupported {
  Future<UsageReport> usage() =>
      Future.error(const CortexApiException('这个后端没有用量端点。', statusCode: 404));
}

mixin RunAttachUnsupported {
  Stream<ChatEvent> attachChat(String sessionId) => Stream<ChatEvent>.error(
    const CortexApiException('这个后端没有可重挂的轮次。', statusCode: 404),
  );
}

/// 给测试替身用：这个后端没有技能。
mixin SkillsUnsupported {
  static const _absent = CortexApiException('这个后端没有技能。', statusCode: 404);

  /// 列表回**空**而不是抛 —— 与 `AssistantsUnsupported` 同一条理由。
  Future<List<Skill>> skills() async => const [];

  Future<Skill> createSkill(Skill draft) async => throw _absent;

  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) async => throw _absent;

  Future<void> deleteSkill(String id) async => throw _absent;
}

/// 给测试替身用：这个后端没有智能体。
mixin AssistantsUnsupported {
  static const _absent = CortexApiException('这个后端没有智能体。', statusCode: 404);

  /// 列表回**空**而不是抛：一个「还没建过智能体」的账号本来就是空的，
  /// 而用这个 mixin 的替身多半只是不关心这一块 —— 抛错会让每个测试
  /// 都被迫处理一个与它无关的异常。与 `ImagesUnsupported` 同一条理由。
  Future<List<Assistant>> assistants() async => const [];

  // 下面三个是**动作**：回一个假的「成功了」等于骗调用方
  Future<Assistant> createAssistant(Assistant draft) async => throw _absent;

  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,
    List<String>? disabledTools,
  }) async => throw _absent;

  Future<void> deleteAssistant(String id) async => throw _absent;
}

/// 给测试替身用：这个后端不画图，也没有画廊。
mixin ImagesUnsupported {
  static const _absent = CortexApiException('这个后端不画图。', statusCode: 404);

  Future<BlobUrl> blobUrl(String hash) async => throw _absent;

  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  }) async => throw _absent;

  /// 画廊回**空**而不是抛：一个「还没画过图」的账号本来就是空的，
  /// 而这里的替身多半只是不关心图片这一块。抛错会让每个用它的测试
  /// 都被迫处理一个与它无关的异常。同理文件夹。
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) async => const Gallery();

  Future<Folders> folders() async => const Folders();

  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
  }) async => const LibraryPage();

  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  }) async => throw _absent;

  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  }) async => throw _absent;

  Future<void> removeFromLibrary(String id) async => throw _absent;

  // 下面这些都是**动作**，不是查询 —— 回一个「成功了」等于骗调用方。
  // 空列表与「没画过图」讲得通，「分享成功但没有链接」讲不通
  Future<String> shareImage(String id) async => throw _absent;

  Future<void> unshareImage(String id) async => throw _absent;

  Future<void> removeImage(String id) async => throw _absent;

  Future<Folders> createFolder(String name) async => throw _absent;

  Future<Folders> renameFolder(String id, String name) async => throw _absent;

  Future<Folders> deleteFolder(String id) async => throw _absent;

  Future<void> moveImage(String id, String? folderId) async => throw _absent;
}

mixin LocalMcpUnsupported {
  static const _absent = CortexApiException('这个后端没有本机 MCP。', statusCode: 404);

  Future<McpConfigView> mcpConfig() async => throw _absent;

  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  }) async => throw _absent;

  Future<McpConfigView> deleteMcpServer(String name) async => throw _absent;

  Future<McpConfigView> reloadMcp() async => throw _absent;

  Future<List<McpParsedServer>> parseMcpPaste(String text) async =>
      throw _absent;

  Future<List<McpRegistryEntry>> searchMcpRegistry(String query) async =>
      throw _absent;
}

/// 同上，给答不了 `GET /sandbox/health` 的替身用：**一律「这里没有沙箱」**。
///
/// mock 数据源、回放替身、测试里那几个假 API 都属于这一类 —— 它们身后
/// 没有任何编排服务。回 [SandboxHealth.absent] 而不是假装 `ready`：
/// 假装的后果是连接页承诺一项这个后端给不了的能力，而用户要发一句话
/// 才发现（那正是这条探测存在的理由）。
///
/// 与那几个同一条禁令：**不要用在真实客户端上**。用了之后，
/// `HttpCortexApi` 漏实现这个方法不会编译报错，而是静默退化成
/// 「这个部署没有沙箱」—— 症状是云端对话的能力说明永远显示不可用。
mixin SandboxHealthUnsupported {
  Future<SandboxHealth> sandboxHealth() async => SandboxHealth.absent;
}

mixin ModelSourcesUnsupported {
  Future<ModelSources> modelSources() async => const ModelSources();

  /// 联网检索的配置也在这个 mixin 里 —— 它与模型来源同一族（都是
  /// 「这个部署配了什么」），而各家测试替身没有理由分别关掉其中一个。
  Future<SearchPrefs> searchPrefs() async => const SearchPrefs();

  Future<SearchPrefs> saveSearchPrefs({
    String? provider,
    String? apiKey,
    String? baseUrl,
    int? maxResults,
    String? depth,
    int? cutoffLimit,
    List<String>? excludeDomains,
  }) async => const SearchPrefs();

  Future<ModelSources> saveModelSource({
    String? id,
    required String provider,
    String apiKey = '',
    String label = '',
    String? baseUrl,
    bool? enabled,
    List<String>? models,
  }) async => const ModelSources();

  Future<ModelSources> deleteModelSource(String id) async =>
      const ModelSources();

  Future<FetchedModels> fetchSourceModels(String id) async =>
      const FetchedModels();

  Future<SourceCheck> checkModelSource(String id, {String model = ''}) async =>
      const SourceCheck(ok: false, detail: '这个部署不支持连通性检查');

  Future<ModelSources> saveModelCaps(
    String sourceId,
    String modelId,
    CapsOverride caps,
  ) async => const ModelSources();

  Future<RoleAssignments> modelRoles() async => const RoleAssignments();

  Future<RoleAssignments> saveModelRoles(RoleAssignments roles) async => roles;
}
