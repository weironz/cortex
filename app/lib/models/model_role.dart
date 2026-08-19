/// 默认模型 —— 把角色指派给「哪条来源的哪个型号」。
///
/// # 它填的是中间那一层
///
/// 在它之前只有两头：部署配的那个（服务端环境变量），和用户**逐轮**的
/// 选择（撰写框下面那个 chip）。「我这个账号平时默认用哪个」没有地方存。
///
/// 逐轮选择替代不了它 —— 那是「这一句用哪个」，关掉窗口就该忘。而
/// 「快速模型」那一档（后台抽取、会话命名）**根本不经过用户**：
/// 它此前只能用部署配的那个，一个自带 key 的人没有任何办法让这些调用
/// 走自己的账户。
///
/// # 三个角色，不是四个
///
/// Cherry Studio 那一页有「翻译模型」。我们没有翻译功能 ——
/// 摆一个没人调用的角色出来就是又一次「造好了没人用」，
/// 而用户会以为自己配的东西在起作用。
library;

import 'json.dart';

/// 一个角色。
///
/// 线上是字符串（`main` / `cheap` / `image`），**与服务端
/// `ModelRole::as_str` 是同一套写法**。拼错的表现是静默不生效：
/// 存进去了、服务端读出来认不出、于是回落到部署那个，
/// 而这一页上那一栏显示得好好的。
enum ModelRole {
  main('main', '主模型', '对话默认用它。撰写框里选了别的，那一句以那个为准。'),
  cheap('cheap', '快速模型', '后台杂活用它：会话命名、内容抽取。这些调用不经过你，所以只能在这里配。'),
  image('image', '绘画模型', '让它画图时用它。不指派的话，自动挑一个最便宜的能生图的。');

  const ModelRole(this.wire, this.label, this.hint);

  /// 线上写法。
  final String wire;
  final String label;
  final String hint;

  /// 从线上写法读回来。**认不出给 `null`** —— 调用方据此忽略，
  /// 而不是猜一个。
  static ModelRole? parse(String? s) {
    for (final r in ModelRole.values) {
      if (r.wire == s) return r;
    }
    return null;
  }
}

/// 一条指派。
class RoleAssignment {
  const RoleAssignment({
    required this.role,
    required this.source,
    required this.model,
  });

  final ModelRole role;

  /// `model_sources.id`，或字面量 `deployment`。
  ///
  /// **不能省。** 同一个型号名可以在两条来源上都有（两个 OpenAI 兼容
  /// 网关），而它们用的是不同的 key、不同的端点、不同的账单。
  final String source;

  final String model;

  Map<String, Object?> toJson() => {
    'role': role.wire,
    'source': source,
    'model': model,
  };
}

/// `GET/PUT /settings/model-roles` 的形状。
///
/// **整份替换**：角色只有三个，界面上是同一屏三行。整份发少一套
/// 「哪个变了」的增量协议，也少一个「清空某个角色」的特例 ——
/// 不在列表里就是没指派。
class RoleAssignments {
  const RoleAssignments({this.roles = const []});

  factory RoleAssignments.fromJson(Map<String, dynamic> json) =>
      RoleAssignments(
        roles: asObjectList(json['roles'])
            .map((e) {
              final role = ModelRole.parse(e['role'] as String?);
              // 认不出的角色**直接忽略**，不报错：那是一个比这个客户端新的
              // 版本写进去的，而忽略一个不认识的偏好比让整页打不开好
              if (role == null) return null;
              return RoleAssignment(
                role: role,
                source: asString(e['source']),
                model: asString(e['model']),
              );
            })
            .nonNulls
            .toList(growable: false),
      );

  final List<RoleAssignment> roles;

  /// 这个角色指派的是什么。`null` = 没指派。
  RoleAssignment? of(ModelRole role) {
    for (final a in roles) {
      if (a.role == role) return a;
    }
    return null;
  }

  /// 改一条（`assignment == null` = 清掉这个角色），返回新的一份。
  ///
  /// **不可变地改**：界面上三行共用一份状态，就地改的话
  /// 「保存失败要回滚」就没有原件可回。
  RoleAssignments with_(ModelRole role, RoleAssignment? assignment) {
    final out = roles.where((a) => a.role != role).toList();
    if (assignment != null) out.add(assignment);
    // 按枚举顺序排：服务端整份替换后原样回显，不排的话每次保存完
    // 三行的顺序都可能跳一下
    out.sort((a, b) => a.role.index.compareTo(b.role.index));
    return RoleAssignments(roles: out);
  }

  Map<String, Object?> toJson() => {
    'roles': roles.map((a) => a.toJson()).toList(growable: false),
  };
}
