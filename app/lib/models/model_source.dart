/// 一条模型来源 —— 供应商 + key + 端点 + 它开放的型号。
///
/// # 它取代了三个概念
///
/// 从前设置里有三样东西：「跟随部署」「自己的 API key」「本机模型」。
/// 用户的原话是「上来就给四种选择，太难理解了」「第一步都是添加模型，
/// 不分云端本地，离线在线」。
///
/// 它们结构上本来就是同一个东西（`{provider, key, base_url}`），
/// 只是**存储位置**不同。位置这件事用户不该关心，所以现在只有一份列表。
///
/// 部署提供的那条也在里面（`builtin`），只读、不可删 ——
/// 藏起来的话，一个零配置就能聊的新用户会以为自己什么都没有。
library;

import 'json.dart';

/// 「部署提供」那条的 id。**与服务端 `DEPLOYMENT_SOURCE_ID` 是同一个常量。**
///
/// 拼错的表现是静默走错来源：请求带着一个服务端不认识的 id，
/// 服务端回落到部署那条 —— 用户以为在用自己的 key，账单却记在配额上。
const String kDeploymentSource = 'deployment';

class ModelSource {
  const ModelSource({
    required this.id,
    required this.provider,
    this.label = '',
    this.keyTail,
    this.baseUrl,
    this.enabled = true,
    this.models = const [],
    this.catalog = const [],
    this.builtin = false,
    this.freeOfQuota = false,
  });

  factory ModelSource.fromJson(Map<String, dynamic> json) => ModelSource(
    id: asString(json['id']),
    provider: asString(json['provider']),
    label: asString(json['label']),
    keyTail: json['key_tail'] as String?,
    baseUrl: json['base_url'] as String?,
    enabled: json['enabled'] != false,
    models: asStringList(json['models']),
    catalog: asObjectList(
      json['catalog'],
    ).map(FetchedModel.fromJson).toList(growable: false),
    builtin: json['builtin'] == true,
    freeOfQuota: json['free_of_quota'] == true,
  );

  final String id;

  /// 供应商 id（`deepseek` / `anthropic` / …）。
  final String provider;

  /// 用户给这条起的名字。空 = 用供应商的显示名。
  ///
  /// 同一家可以配两条（两个网关、两个账号），靠它分得清。
  final String label;

  /// 明文 key 的后 4 位。`null` = 部署那条（那把 key 不是用户的）。
  final String? keyTail;

  /// 自建端点。`null` = 用供应商官方那个。
  final String? baseUrl;

  /// 关掉的来源不进选择器，但配置留着 ——
  /// 删掉再填一遍 key 是最烦的一种「临时关掉」。
  final bool enabled;

  /// 这条来源**开放**哪些型号。空 = 还没拉过（界面提示去点「获取模型列表」）。
  final List<String> models;

  /// 最近一次「获取模型列表」拉到的**全部**型号，带能力与价目。
  ///
  /// [models] 是它的子集，两者的差就是界面上那个「未启用」分组。
  ///
  /// **可能是空的**，两种情况：还没拉过，或者对面是**老服务端**
  /// （2026-08-21 之前的版本不下发这个字段）。界面别直接读它，
  /// 读 [shownCatalog]。
  final List<FetchedModel> catalog;

  /// 界面该画哪些型号。
  ///
  /// # ⚠️ 为什么不能直接用 [catalog]
  ///
  /// 老服务端不认识这个字段，回来就是空的 —— 而那台机器上这条来源
  /// **明明配着几个型号**（`models` 里有）。直接画 `catalog` 的结果是
  /// 「还没有型号，点获取模型列表」，而用户上一分钟还在用它们聊天。
  ///
  /// 一个新客户端连上老服务端就让人以为配置丢了，是升级里最不该有的
  /// 表现：他会去重填，而重填并不能修好一个根本没坏的东西。
  ///
  /// 兜底出来的条目**只有 id**，能力与价目一概为 null —— 界面据此留白，
  /// 不编数字（那正是三态里 `null` 的用法）。
  List<FetchedModel> get shownCatalog {
    if (catalog.isNotEmpty) return catalog;
    return [for (final id in models) FetchedModel(id: id)];
  }

  /// 这个型号此刻开着吗。
  ///
  /// 判据只有一处：**在 [models] 里就是开着的**。不另存一位布尔 ——
  /// 同一件事两处表达，迟早不一致，而症状是「界面上关着、选择器里还在」。
  bool isEnabled(String modelId) => models.contains(modelId);

  /// 这条来源自己填了端点（中转站 / 公司网关 / one-api / 自建 vLLM）。
  ///
  /// **与服务端 `model_sources::is_custom_endpoint` 是同一条判据**，
  /// 两边必须一字不差：这一位决定了目录里那些能力还算不算数（端点后面是谁
  /// 我们一无所知），也决定了生图走哪条协议。判歪的表现是界面上说「支持」
  /// 而实际打过去 404，或者反过来 —— 明明能画却一个候选都不给
  /// （2026-08-21 报的就是后者）。
  bool get isCustom =>
      provider == 'custom' || (baseUrl?.trim().isNotEmpty ?? false);

  /// 部署提供的那条：只读、不可删。
  final bool builtin;

  /// 用它不计配额（自带 key 的都不计）。
  final bool freeOfQuota;

  /// 界面上叫什么。
  String displayName(String fallback) =>
      label.isNotEmpty ? label : (fallback.isNotEmpty ? fallback : provider);
}

/// 可以添加哪几家（服务端下发）。
class ProviderChoice {
  const ProviderChoice({
    required this.id,
    required this.displayName,
    this.description = '',
    this.baseUrl = '',
    this.requiresAuth = true,
  });

  factory ProviderChoice.fromJson(Map<String, dynamic> json) => ProviderChoice(
    id: asString(json['id']),
    displayName: asString(json['display_name']),
    description: asString(json['description']),
    baseUrl: asString(json['base_url']),
    requiresAuth: json['requires_auth'] != false,
  );

  final String id;
  final String displayName;
  final String description;

  /// 官方端点。界面拿它当「留空 = 连到这里」的提示 ——
  /// 一个空的端点输入框说不出留空会去哪。
  final String baseUrl;

  /// 要不要 key。Ollama 这类是 false，界面据此不逼用户填一把他没有的密钥。
  final bool requiresAuth;
}

/// `GET /settings/model-sources` 的响应。
class ModelSources {
  const ModelSources({
    this.sources = const [],
    this.canAdd = false,
    this.providers = const [],
  });

  factory ModelSources.fromJson(Map<String, dynamic> json) => ModelSources(
    sources: asObjectList(
      json['sources'],
    ).map(ModelSource.fromJson).toList(growable: false),
    canAdd: json['can_add'] == true,
    providers: asObjectList(
      json['providers'],
    ).map(ProviderChoice.fromJson).toList(growable: false),
  );

  final List<ModelSource> sources;

  /// 这个部署能不能存自带 key（服务端没配主密钥就是 false）。
  ///
  /// **如实说** —— 给一个存不进去的表单，用户会填、会点保存、会以为成了，
  /// 下次打开又是空的。
  final bool canAdd;

  final List<ProviderChoice> providers;

  ModelSource? byId(String id) {
    for (final s in sources) {
      if (s.id == id) return s;
    }
    return null;
  }

  ProviderChoice? providerOf(String id) {
    for (final p in providers) {
      if (p.id == id) return p;
    }
    return null;
  }

  /// 这条来源在界面上叫什么（拿供应商显示名兜底）。
  String nameOf(ModelSource s) =>
      s.displayName(providerOf(s.provider)?.displayName ?? '');

  /// 能挑的型号，按来源分组。**只算启用了的**。
  Iterable<ModelSource> get usable => sources.where((s) => s.enabled);
}

/// 一个可加入的型号 —— 名字 + 它能干什么。
///
/// 能力字段一律可空：**「不知道」与「不行」是两回事**。目录里查不到的
/// 型号三个字段都是 null，界面据此说「不知道」，而不是画一个看起来像
/// 「不支持」的灰徽标。
/// 用户在这条来源上**手工按下**的能力位。
///
/// # 为什么每一位都是三态
///
/// `null` = 「这一位我没意见，按自动的来」，与「我明确说了不支持」必须
/// 分得开。混在一起的话，一个只想改 vision 的人会把其余几位一起按成
/// 「不支持」—— 而那正是这个开关要解决的问题的反面。
///
/// Cherry Studio 那一版的能力 chip 是**二值**的（点亮 = 支持，不亮 = 不支持）。
/// 我们不能照抄：这个仓库为了「不知道 ≠ 不支持」这条约定改过三处判据
/// （放行、筛选、徽标），二值开关会把它整个抹掉。
class CapsOverride {
  const CapsOverride({
    this.displayName,
    this.context,
    this.toolCall,
    this.vision,
    this.imageOutput,
    this.reasoning,
  });

  factory CapsOverride.fromJson(Map<String, dynamic> json) => CapsOverride(
    displayName: json['display_name'] as String?,
    context: asIntOrNull(json['context']),
    toolCall: json['tool_call'] as bool?,
    vision: json['vision'] as bool?,
    imageOutput: json['image_output'] as bool?,
    reasoning: json['reasoning'] as bool?,
  );

  final String? displayName;
  final int? context;
  final bool? toolCall;
  final bool? vision;
  final bool? imageOutput;
  final bool? reasoning;

  /// 一位都没按 —— 服务端据此把整条删掉而不是留个空壳。
  bool get isEmpty =>
      displayName == null &&
      context == null &&
      toolCall == null &&
      vision == null &&
      imageOutput == null &&
      reasoning == null;

  /// ⚠️ **只发按过的那几位。** 把 null 也发上去的话，服务端分不出
  /// 「没意见」与「明确说不支持」—— 而那两件事在这个仓库里差得很远。
  Map<String, Object?> toJson() => {
    if (displayName != null) 'display_name': displayName,
    if (context != null) 'context': context,
    if (toolCall != null) 'tool_call': toolCall,
    if (vision != null) 'vision': vision,
    if (imageOutput != null) 'image_output': imageOutput,
    if (reasoning != null) 'reasoning': reasoning,
  };

  CapsOverride copyWith({
    Object? displayName = _keep,
    Object? context = _keep,
    Object? toolCall = _keep,
    Object? vision = _keep,
    Object? imageOutput = _keep,
    Object? reasoning = _keep,
  }) => CapsOverride(
    displayName: displayName == _keep
        ? this.displayName
        : displayName as String?,
    context: context == _keep ? this.context : context as int?,
    toolCall: toolCall == _keep ? this.toolCall : toolCall as bool?,
    vision: vision == _keep ? this.vision : vision as bool?,
    imageOutput: imageOutput == _keep ? this.imageOutput : imageOutput as bool?,
    reasoning: reasoning == _keep ? this.reasoning : reasoning as bool?,
  );
}

/// `copyWith` 的哨兵：**要能把一位改回 null**（「没意见」），
/// 而默认参数为 null 的写法表达不了「传了 null」与「没传」的区别 ——
/// 那正是这个类型存在的全部理由，在它自己的 copyWith 上栽了就很讽刺。
const Object _keep = Object();

class FetchedModel {
  const FetchedModel({
    required this.id,
    this.displayName = '',
    this.context,
    this.toolCall,
    this.vision,
    this.imageOutput,
    this.imageUnwired = false,
    this.customEndpoint = false,
    this.reasoning,
    this.inputMicrosPerMtok,
    this.outputMicrosPerMtok,
    this.overridden = const CapsOverride(),
  });

  factory FetchedModel.fromJson(Map<String, dynamic> json) => FetchedModel(
    id: asString(json['id']),
    displayName: asString(json['display_name']),
    context: asIntOrNull(json['context']),
    toolCall: json['tool_call'] as bool?,
    vision: json['vision'] as bool?,
    imageOutput: json['image_output'] as bool?,
    imageUnwired: json['image_unwired'] == true,
    customEndpoint: json['custom_endpoint'] == true,
    reasoning: json['reasoning'] as bool?,
    inputMicrosPerMtok: asIntOrNull(json['input_micros_per_mtok']),
    outputMicrosPerMtok: asIntOrNull(json['output_micros_per_mtok']),
    overridden: CapsOverride.fromJson(
      json['overridden'] as Map<String, dynamic>? ?? const {},
    ),
  );

  final String id;
  final String displayName;
  final int? context;

  /// **支持工具调用吗。** 筛选里最要紧的一位 —— 不支持的模型跑 agent
  /// 会流畅地回答而一个工具都不调，界面上看不出任何异常。
  final bool? toolCall;

  /// 看得懂图吗（视觉输入）。与 [imageOutput] 是两件事。
  final bool? vision;

  /// 能生图吗。这一位的含义是「点了能不能出图」，不是「理论上会不会画」
  /// —— 协议没接的那几家一律 false。
  final bool? imageOutput;

  /// 「它会画，但**我们**还没接这家」。
  ///
  /// [imageOutput] 把这种情况压成了 false，而 false 在界面上读作
  /// 「这模型不会画画」—— 错的，且把责任推给了模型。2026-08-20 用户
  /// 就是因此来问「为什么这些模型不支持」：搜出一屏 `gemini-*-image`，
  /// 筛选栏却写着「能生图 0」。
  final bool imageUnwired;

  /// 这条来源指向的是**它自己的端点**（中转站 / 公司网关 / one-api / 自建），
  /// 或者供应商就是「自定义」。
  ///
  /// 一为真，上面那些能力就**不是断言**：目录描述的是厂商官方接口，
  /// 而端点后面是谁我们一无所知 —— 界面据此不拦，只把目录里的话当提醒。
  ///
  /// 2026-08-20 实测：一个中转站把 `gpt-image-2` 包装成普通聊天，
  /// 零代码就能跑，而我们照着目录把它画成灰的、还说「我们没接这家」。
  final bool customEndpoint;

  final bool? reasoning;
  final int? inputMicrosPerMtok;
  final int? outputMicrosPerMtok;

  /// 这个模型上用户手工按过的那几位。界面据此画出「这一位是你改的」——
  /// 不带的话，一个改过 vision 的人下次打开只看到一个 `true`，分不清
  /// 那是目录说的还是他自己按的，于是不敢动。
  final CapsOverride overridden;

  String get label => displayName.isEmpty ? id : displayName;
}

/// `POST .../{id}/check` 的响应 —— 拿存下来的 key 真发一次请求的结果。
///
/// **失败也是 200**：这条端点的产出是「一次检查的结论」，不是「请求成功了
/// 没有」。用 4xx 表达「你的 key 不对」会与「这条端点不存在」「你没登录」
/// 混在一起，而客户端已经有一整套按状态码分支的逻辑，那些分支会把一次
/// 正常的检查结果当成故障处理。
class SourceCheck {
  const SourceCheck({required this.ok, required this.detail});

  factory SourceCheck.fromJson(Map<String, dynamic> json) =>
      SourceCheck(ok: json['ok'] == true, detail: asString(json['detail']));

  final bool ok;

  /// 一句给人看的话。**通过时也有** —— 「通过」两个字回答不了
  /// 「我刚才到底验了什么」，而那正是点这个按钮的人想知道的。
  final String detail;
}

/// `POST .../{id}/models` 的响应。
class FetchedModels {
  const FetchedModels({this.models = const [], this.live = false, this.note});

  factory FetchedModels.fromJson(Map<String, dynamic> json) => FetchedModels(
    models: asObjectList(
      json['models'],
    ).map(FetchedModel.fromJson).toList(growable: false),
    live: json['live'] == true,
    note: json['note'] as String?,
  );

  final List<FetchedModel> models;

  /// 真是从供应商拉的吗。`false` = 内置回落，界面**必须**说出来 ——
  /// 悄悄回落的表现是「我点了获取列表，它给了我一份看起来像样的、
  /// 但其实是编译期写死的清单」。
  final bool live;

  final String? note;
}
