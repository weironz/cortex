import 'json.dart';

/// 一个模型在这个窗口里的用量与花费。
class ModelUsage {
  const ModelUsage({
    required this.model,
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.costMicros,
  });

  factory ModelUsage.fromJson(Map<String, dynamic> json) => ModelUsage(
    model: asString(json['model']),
    inputTokens: asInt(json['input_tokens']),
    outputTokens: asInt(json['output_tokens']),
    costMicros: asIntOrNull(json['cost_micros']),
  );

  final String model;
  final int inputTokens;
  final int outputTokens;

  /// 花了多少微元。`null` = **这个部署没有它的价目**，不是零。
  ///
  /// 界面必须把这两者画成不同的东西：「¥0.00」读起来是免费，
  /// 而事实是「我们不知道这个模型多少钱」。
  final int? costMicros;

  int get totalTokens => inputTokens + outputTokens;
}

/// `GET /auth/usage` —— 这个窗口用了多少、花了多少、还剩多少。
class UsageReport {
  const UsageReport({
    this.usedTokens = 0,
    this.ownKeyTokens = 0,
    this.limitTokens,
    this.remainingTokens,
    this.windowDays = 30,
    this.costMicros = 0,
    this.currency = 'CNY',
    this.unpricedTokens = 0,
    this.byModel = const [],
  });

  factory UsageReport.fromJson(Map<String, dynamic> json) => UsageReport(
    usedTokens: asInt(json['used_tokens']),
    ownKeyTokens: asInt(json['own_key_tokens']),
    limitTokens: asIntOrNull(json['limit_tokens']),
    remainingTokens: asIntOrNull(json['remaining_tokens']),
    windowDays: asInt(json['window_days'], 30),
    costMicros: asInt(json['cost_micros']),
    currency: asString(json['currency'], 'CNY'),
    unpricedTokens: asInt(json['unpriced_tokens']),
    byModel: asObjectList(
      json['by_model'],
    ).map(ModelUsage.fromJson).toList(growable: false),
  );

  /// 占配额的那部分（服务端那把 key）。
  final int usedTokens;

  /// 自带 key 花掉的。单独一项 —— 它回答「我花了多少」，
  /// 而不是「我还剩多少额度」。
  final int ownKeyTokens;

  /// `null` = 这个部署不限量（自托管单人用时那是合理的形态）。
  final int? limitTokens;

  /// 服务端算好的剩余。**不在客户端做 `limit - used`**：
  /// 不限量时那个减法会算出一个负数，而每个客户端各写一遍这个判断，
  /// 迟早有一个把「无限」显示成 -3999。
  final int? remainingTokens;

  final int windowDays;

  /// 花了多少微元（百万分之一货币单位）。只含算得出价的那些模型。
  final int costMicros;
  final String currency;

  /// 属于**没有价目的模型**的 token 数。见 [ModelUsage.costMicros]。
  final int unpricedTokens;

  final List<ModelUsage> byModel;

  bool get limited => limitTokens != null;

  /// 用掉的比例，0…1。不限量时是 `null`（进度条那时不该出现）。
  double? get ratio {
    final limit = limitTokens;
    if (limit == null || limit <= 0) return null;
    return (usedTokens / limit).clamp(0.0, 1.0);
  }
}

/// 把微元显示成金额。
///
/// 小额也要看得见：一次对话常常只花几厘，四舍五入到分就永远是 ¥0.00，
/// 而那正是用户会拿来判断「这个功能贵不贵」的数字。所以不足一分时
/// 多给两位。
String formatMoney(int micros, String currency) {
  final symbol = switch (currency.toUpperCase()) {
    'CNY' || 'RMB' => '¥',
    'USD' => r'$',
    'EUR' => '€',
    _ => '$currency ',
  };
  final units = micros / 1000000;
  if (micros == 0) return '$symbol${units.toStringAsFixed(2)}';
  if (units < 0.01) return '$symbol${units.toStringAsFixed(4)}';
  return '$symbol${units.toStringAsFixed(2)}';
}

/// 大数字加千位分隔。`1234567` → `1,234,567`。
///
/// token 数动辄七八位，不分隔的话没人一眼数得清是一百万还是一千万 ——
/// 而那正是「我还能用多久」的答案。
String formatTokens(int n) {
  final s = n.abs().toString();
  final buf = StringBuffer(n < 0 ? '-' : '');
  for (var i = 0; i < s.length; i++) {
    if (i > 0 && (s.length - i) % 3 == 0) buf.write(',');
    buf.write(s[i]);
  }
  return buf.toString();
}
