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

  /// 这条来源开放哪些型号。空 = 还没拉过（界面提示去点「获取模型列表」）。
  final List<String> models;

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
class FetchedModel {
  const FetchedModel({
    required this.id,
    this.displayName = '',
    this.context,
    this.toolCall,
    this.vision,
    this.imageOutput,
    this.reasoning,
    this.inputMicrosPerMtok,
    this.outputMicrosPerMtok,
  });

  factory FetchedModel.fromJson(Map<String, dynamic> json) => FetchedModel(
    id: asString(json['id']),
    displayName: asString(json['display_name']),
    context: asIntOrNull(json['context']),
    toolCall: json['tool_call'] as bool?,
    vision: json['vision'] as bool?,
    imageOutput: json['image_output'] as bool?,
    reasoning: json['reasoning'] as bool?,
    inputMicrosPerMtok: asIntOrNull(json['input_micros_per_mtok']),
    outputMicrosPerMtok: asIntOrNull(json['output_micros_per_mtok']),
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

  final bool? reasoning;
  final int? inputMicrosPerMtok;
  final int? outputMicrosPerMtok;

  String get label => displayName.isEmpty ? id : displayName;
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
