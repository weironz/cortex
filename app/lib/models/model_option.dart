import 'json.dart';

/// 「自动」档在线上的写法。**与服务端 `ModelChoice::AUTO` 是同一个常量。**
///
/// 拼错的表现是**静默退回默认模型** —— 用户以为自己开了自动档，
/// 实际一直在用同一个，而界面上看不出任何区别。所以它在这里只出现一次。
const String kAutoModel = 'auto';

/// 一个可选的模型（`GET /llm/models`）。
///
/// 两个来源拼起来：**能不能用**由这个部署的供应商定义说了算，
/// **能干什么、多少钱**由服务端内置的模型目录说了算。
class ModelOption {
  const ModelOption({
    required this.id,
    required this.displayName,
    this.context,
    this.toolCall,
    this.vision,
    this.reasoning,
    this.inputMicrosPerMtok,
    this.outputMicrosPerMtok,
  });

  factory ModelOption.fromJson(Map<String, dynamic> json) => ModelOption(
    id: asString(json['id']),
    displayName: asString(json['display_name']),
    context: asIntOrNull(json['context']),
    toolCall: json['tool_call'] as bool?,
    vision: json['vision'] as bool?,
    reasoning: json['reasoning'] as bool?,
    inputMicrosPerMtok: asIntOrNull(json['input_micros_per_mtok']),
    outputMicrosPerMtok: asIntOrNull(json['output_micros_per_mtok']),
  );

  /// 填进请求里的那个名字。
  final String id;
  final String displayName;

  /// 上下文窗口。`null` = 服务端目录里查不到这个模型。
  final int? context;

  /// 支持工具调用吗。
  ///
  /// ⚠️ **三态**：`true` / `false` / `null`（不知道）。
  ///
  /// `false` 要拦下来 —— 那样的模型跑 agent 会流畅地回答而一个工具都不调，
  /// 用户看不出哪里不对。而 `null` 只能提醒：目录里没有它，不代表它不行，
  /// 把「不知道」当成 `false` 会把一个能用的模型挡在外面。
  final bool? toolCall;

  final bool? vision;
  final bool? reasoning;

  /// 每百万输入 token 多少**美元微元**。`null` = 目录里没有它的价目。
  final int? inputMicrosPerMtok;
  final int? outputMicrosPerMtok;

  /// 这个模型能不能拿来跑 agent 会话。`null` = 不知道。
  bool? get usableForAgent => toolCall;
}

/// `GET /llm/models` 的响应。
class ModelCatalog {
  const ModelCatalog({
    this.provider = '',
    this.defaultModel = '',
    this.cheapModel = '',
    this.models = const [],
    this.autoAvailable = false,
  });

  factory ModelCatalog.fromJson(Map<String, dynamic> json) => ModelCatalog(
    provider: asString(json['provider']),
    defaultModel: asString(json['default_model']),
    cheapModel: asString(json['cheap_model']),
    models: asObjectList(
      json['models'],
    ).map(ModelOption.fromJson).toList(growable: false),
    autoAvailable: json['auto_available'] == true,
  );

  /// 这个部署连的是哪一家。
  final String provider;

  /// 不选时用的那个。
  final String defaultModel;
  final String cheapModel;

  /// 这个部署开放了哪些。**只能在这里面挑。**
  final List<ModelOption> models;

  /// 支持「自动」档吗。候选不足两个时服务端会报 false ——
  /// 那时它与默认档没有区别，摆出来只会让人以为它在做什么。
  final bool autoAvailable;

  ModelOption? byId(String id) {
    for (final m in models) {
      if (m.id == id) return m;
    }
    return null;
  }
}

/// 把每百万 token 的美元微元显示成人看得懂的价格。
///
/// 单位是**美元**：服务端的模型目录来自 models.dev，价格一律美元。
/// 这里不折算 —— 折算要汇率，而一个随手挑的汇率折出来的价格，用户没法
/// 判断对不对。
String formatPerMtok(int? micros) {
  if (micros == null) return '—';
  final usd = micros / 1000000;
  if (usd == 0) return r'$0';
  if (usd < 0.01) return '\$${usd.toStringAsFixed(4)}';
  return '\$${usd.toStringAsFixed(2)}';
}

/// 上下文窗口的短写法：`128000` → `128K`，`1000000` → `1M`。
String formatContext(int? tokens) {
  if (tokens == null || tokens <= 0) return '—';
  if (tokens >= 1000000) {
    final m = tokens / 1000000;
    return '${m == m.roundToDouble() ? m.toInt() : m.toStringAsFixed(1)}M';
  }
  if (tokens >= 1000) return '${(tokens / 1000).round()}K';
  return '$tokens';
}
