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

part 'cortex_api/core.dart';
part 'cortex_api/chat.dart';
part 'cortex_api/sessions.dart';
part 'cortex_api/projects.dart';
part 'cortex_api/assistants.dart';
part 'cortex_api/auth.dart';
part 'cortex_api/workspace.dart';
part 'cortex_api/mcp.dart';
part 'cortex_api/models.dart';
part 'cortex_api/import.dart';
part 'cortex_api/blobs.dart';
part 'cortex_api/images.dart';
part 'cortex_api/library.dart';
part 'cortex_api/sandbox.dart';
part 'cortex_api/sync.dart';

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
abstract interface class CortexApi
    with
        CortexApiCore,
        CortexApiChat,
        CortexApiSessions,
        CortexApiProjects,
        CortexApiAssistants,
        CortexApiSkills,
        CortexApiAuth,
        CortexApiWorkspace,
        CortexApiMcp,
        CortexApiModels,
        CortexApiImport,
        CortexApiBlobs,
        CortexApiImages,
        CortexApiLibrary,
        CortexApiSandbox,
        CortexApiSync {}

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

  /// 账号资料那几条一律 404 —— 与 `whoAmI()` 回 null 是同一句话的两种说法：
  /// **这个后端没有账号体系**。
  ///
  /// 抛 404 而不是回一个空 [Profile]：空资料会让账号页画出一堆点了没反应的
  /// 输入框，而 404 会让它显示那句解释（见 `AccountPage`）。
  static const _absent = CortexApiException('这个后端没有账号体系。', statusCode: 404);

  Future<Profile> profile() async => throw _absent;

  Future<Profile> updateProfile({Patch<String?>? nickname}) async =>
      throw _absent;

  Future<Profile> putAvatar(Uint8List bytes) async => throw _absent;

  Future<Profile> deleteAvatar() async => throw _absent;

  Future<Uint8List> avatarBytes(String userId) async => throw _absent;

  Future<void> changePassword(String oldPassword, String newPassword) async =>
      throw _absent;

  Future<Profile> deleteAccount(String password) async => throw _absent;

  Future<Profile> restoreAccount() async => throw _absent;

  Future<void> startEmailBinding(String email) async => throw _absent;

  Future<Profile> verifyEmail(String code) async => throw _absent;

  Future<Profile> unbindEmail(String password) async => throw _absent;
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
  Future<LocalAttach> localAttach() =>
      Future.error(const CortexApiException('这个后端没有远程接入开关。', statusCode: 404));
  Future<LocalAttach> setLocalAttach(bool enabled) =>
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
    String? sort,
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

/// `GET/PUT /local/attach` 的回答。
class LocalAttach {
  const LocalAttach({required this.enabled, required this.machineHint});

  /// 这台机器现在接不接受远程接入。
  final bool enabled;

  /// 这台机器叫什么 —— **只为让那张卡片认得出自己**。
  ///
  /// 「我的机器」那一页同时画着这张卡片和整张在线名册，而名册里也有这一台。
  /// 不带名字的话，卡片写「这台机器」、名册里写「WILLOPTPC」，用户看不出
  /// 它们是同一台（实机上看了一眼才发现的）。
  ///
  /// ⚠️ **不拿它去认名册里的哪一行**：`machine_hint` 是提示不是身份，
  /// 两台机器重名时那种「标出是哪一行」的做法会指错，且指错时没有征兆。
  final String machineHint;
}
