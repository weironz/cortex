/// 智能体 —— 一份可复用的「人设 + 默认模型」。
///
/// # 名字：界面上叫「智能体」，代码里叫 assistant
///
/// 别家各叫各的：专家（workbuddy）、智能体小助手（Cherry Studio）、
/// 助理（LobeHub）、搭档（chatbox）。是同一个东西。
///
/// ⚠️ **代码里不能叫 agent** —— 这个仓库里「agent」已经是另一个东西：
/// 跑在用户机器上的 `cortex-local` 进程。服务端那条 `/agents` 是它的
/// 心跳注册，加这个功能时两者撞在一起，axum 直接 panic 了（实测）。
library;

import 'json.dart';

class Assistant {
  const Assistant({
    required this.id,
    required this.name,
    this.description = '',
    this.instructions = '',
    this.icon = '',
    this.model,
    this.source,
    this.createdAt,
    this.updatedAt,
  });

  factory Assistant.fromJson(Map<String, dynamic> json) => Assistant(
    id: asString(json['id']),
    name: asString(json['name'], '未命名'),
    description: asString(json['description']),
    instructions: asString(json['instructions']),
    icon: asString(json['icon']),
    model: json['model'] as String?,
    source: json['source'] as String?,
    createdAt: asDateOrNull(json['created_at']),
    updatedAt: asDateOrNull(json['updated_at']),
  );

  final String id;
  final String name;

  /// 一句话说明，列表里给人看。
  final String description;

  /// 人设本身。**它替换默认那句人设**，不是追加 —— 服务端那侧
  /// （`system_prompt_for`）就是这么拼的。
  final String instructions;

  /// 一个 emoji。空 = 界面自己挑一个默认图标。
  final String icon;

  /// 默认用哪个模型。`null` = 跟着用户当下选的走。
  final String? model;

  /// 那个模型属于哪条来源。与 [model] 成对。
  final String? source;

  final DateTime? createdAt;
  final DateTime? updatedAt;

  /// 这个智能体真的说了点什么吗。
  ///
  /// 空人设**等于没有智能体**：拿它去替换默认那句的话，模型得到的是
  /// 一段没有身份描述的提示词，比默认那句更糟。服务端也有同一条判据
  /// （`AssistantBrief::is_meaningful`）—— 两边一致是有意的：客户端这条
  /// 让界面能提前说清楚，服务端那条是最后一道闸。
  bool get isMeaningful => instructions.trim().isNotEmpty;

  Assistant copyWith({
    String? name,
    String? description,
    String? instructions,
    String? icon,
    String? model,
    String? source,
  }) => Assistant(
    id: id,
    name: name ?? this.name,
    description: description ?? this.description,
    instructions: instructions ?? this.instructions,
    icon: icon ?? this.icon,
    model: model ?? this.model,
    source: source ?? this.source,
    createdAt: createdAt,
    updatedAt: updatedAt,
  );

  /// 发给 `/chat` 的那一份 —— **只带模型真的要看的两样**。
  ///
  /// 不带 id / 图标 / 说明：`cortex-local` 拿它们做不了任何事，
  /// 而每一轮都要走一遍网络。
  Map<String, dynamic> toBrief() => {
    'name': name,
    'instructions': instructions,
  };

  @override
  bool operator ==(Object other) =>
      other is Assistant &&
      other.id == id &&
      other.name == name &&
      other.description == description &&
      other.instructions == instructions &&
      other.icon == icon &&
      other.model == model &&
      other.source == source;

  @override
  int get hashCode =>
      Object.hash(id, name, description, instructions, icon, model, source);

  @override
  String toString() => 'Assistant($id, $name)';
}
