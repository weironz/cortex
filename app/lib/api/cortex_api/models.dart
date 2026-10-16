part of '../cortex_api.dart';

mixin CortexApiModels {
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
}
