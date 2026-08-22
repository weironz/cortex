/// 技能 —— 一份写好的做法，模型**要用的时候才取正文**。
///
/// # 两层
///
/// | 层 | 内容 | 什么时候进上下文 |
/// |---|---|---|
/// | 目录 | [name] + [description] | **每一轮**，在系统提示词里 |
/// | 正文 | [instructions] | 模型调了 `load_skill` 之后 |
///
/// 全塞进提示词也能跑，而且只有一两条时更省事。撑不住的是第十条：
/// 系统提示词是可缓存前缀的头，十份做法全塞进去等于每一轮都为那九份用不上
/// 的付钱。
///
/// # 与智能体的分工
///
/// 智能体（人设）回答「**你是谁**」，技能回答「**这件事怎么做**」。
/// 一个人设可以配零到多份做法，反过来也一样 —— 所以它们是两张表，
/// 不是一张表的两列。
library;

import 'json.dart';

class Skill {
  const Skill({
    required this.id,
    required this.name,
    this.description = '',
    this.instructions = '',
    this.enabled = true,
    this.createdAt,
    this.updatedAt,
  });

  factory Skill.fromJson(Map<String, dynamic> json) => Skill(
    id: asString(json['id']),
    name: asString(json['name'], '未命名'),
    description: asString(json['description']),
    instructions: asString(json['instructions']),
    // 缺字段读成**开着**：老服务端没有这一列时，把技能一律读成关掉的话，
    // 用户会发现自己的技能全都失效了 —— 而他什么都没改
    enabled: json['enabled'] as bool? ?? true,
    createdAt: asDateOrNull(json['created_at']),
    updatedAt: asDateOrNull(json['updated_at']),
  );

  final String id;

  /// 模型看见、并且用来取正文的那个名字。
  ///
  /// ⚠️ **它是标识符，不只是标签**：服务端上带 UNIQUE。两条同名技能会让
  /// `load_skill` 静默地取到其中一条，而另一条的做法从此再也不会被执行。
  final String name;

  /// 一句话说明。
  ///
  /// ⚠️ **这一句是给模型做判断用的**，不只是列表副标题：模型就靠它决定这一轮
  /// 该不该把正文取回来。写得含糊（「一些工具」）的下场是技能永远不被取用，
  /// 而没有任何报错。界面上要把这件事说清楚。
  final String description;

  /// 正文：真正的做法。只在 `load_skill` 之后才进上下文。
  final String instructions;

  /// 关掉的技能既不进目录也取不回来。
  final bool enabled;

  final DateTime? createdAt;
  final DateTime? updatedAt;

  /// 这一条进得了目录吗。
  ///
  /// 关掉的进不了。没名字的也进不了 —— 模型没法用空字符串去 `load_skill`。
  /// 没说明的**可以**进：名字本身往往就说明了用途（「周报」），把它挡在外面
  /// 等于让用户配了一个永远不出现的技能。服务端有同一条判据
  /// （`SkillBrief::is_listable`）—— 两边一致是有意的。
  bool get isListable => enabled && name.trim().isNotEmpty;

  /// 发给 `/chat` 的那一份 —— **名字与说明，没有正文**。
  ///
  /// 带上正文就等于取消了分层：贵的那一半又变成每轮都发了。
  Map<String, dynamic> toBrief() => {'name': name, 'description': description};

  Skill copyWith({
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) => Skill(
    id: id,
    name: name ?? this.name,
    description: description ?? this.description,
    instructions: instructions ?? this.instructions,
    enabled: enabled ?? this.enabled,
    createdAt: createdAt,
    updatedAt: updatedAt,
  );

  @override
  bool operator ==(Object other) =>
      other is Skill &&
      other.id == id &&
      other.name == name &&
      other.description == description &&
      other.instructions == instructions &&
      other.enabled == enabled;

  @override
  int get hashCode => Object.hash(id, name, description, instructions, enabled);

  @override
  String toString() => 'Skill($id, $name, enabled: $enabled)';
}
