import 'json.dart';

/// 当前登录的是谁 —— `GET /auth/me` 的回体。
///
/// # 为什么问服务端，而不是登录时把用户名记下来
///
/// 启动时走的是 refresh token 那条路（`AuthController.restoreSession`），
/// 那条路上**根本没有用户名** —— 存下来的只有 token。记在本地的那份会在
/// 第一次重启之后变成一个没人更新的副本：改了用户名不会变，
/// 换了账号登录也不会变，而它就摆在界面左下角。
///
/// 问服务端还顺带答对了另外两种情况：预共享 token 的部署（认不出的 bearer
/// 回落到 owner）、以及关掉认证的部署。
class Account {
  const Account({
    required this.userId,
    required this.username,
    required this.schemaName,
  });

  final String userId;
  final String username;

  /// 这个人的记忆住在哪个 schema。
  ///
  /// 下发出来是为了排障时能一眼对上：「我看到的是不是我自己的库」在多租户
  /// 里是个会被真的问出口的问题。放在账号栏的 tooltip 里。
  final String schemaName;

  factory Account.fromJson(Map<String, dynamic> json) => Account(
    userId: asString(json['user_id']),
    username: asString(json['username']),
    schemaName: asString(json['schema_name']),
  );

  @override
  bool operator ==(Object other) =>
      other is Account &&
      other.userId == userId &&
      other.username == username &&
      other.schemaName == schemaName;

  @override
  int get hashCode => Object.hash(userId, username, schemaName);
}

/// 「这次不动它」与「把它清空」的区别。
///
/// Dart 没有 `Option<Option<T>>`，而 PATCH 语义要这个区别：字段缺席 =
/// 不动，字段是 `null` = 清空。少这一层的话「清空昵称」发不出去 ——
/// 它在线上与「这次没提昵称」长得一模一样。
class Patch<T> {
  const Patch(this.value);
  final T value;
}

/// 账号资料 —— `GET /auth/profile` 的回体。
///
/// 与 [Account] 分开：那一条每次启动都问（我是谁、库在哪），这一条只有
/// 账号页要。
class Profile {
  const Profile({
    required this.userId,
    required this.username,
    this.nickname,
    this.hasAvatar = false,
    this.avatarVersion,
    this.purgeAfter,
  });

  final String userId;

  /// 登录名。**不可改** —— 它是别人引用你的方式。
  final String username;

  /// 显示名。`null` = 没设过。
  ///
  /// **界面回落到 [username] 的判断放在界面层**，不在这里替它做：做了之后
  /// 就分不出「他设了一个恰好等于用户名的昵称」和「他没设过」，而清空
  /// 昵称这个操作要那个区别。
  final String? nickname;

  final bool hasAvatar;

  /// 头像的版本戳。换了头像它就变 —— 界面拿它当缓存键，不然换完还显示旧的。
  final int? avatarVersion;

  /// 这个号正在等着被删，到点真删。`null` = 一切正常。
  ///
  /// 必须显示出来：一个「我以为删掉了」或「我以为撤销了」的误会，
  /// 代价是全部历史。
  final DateTime? purgeAfter;

  /// 界面上该叫他什么。
  String get displayName =>
      (nickname?.trim().isNotEmpty ?? false) ? nickname!.trim() : username;

  factory Profile.fromJson(Map<String, dynamic> json) => Profile(
    userId: asString(json['user_id']),
    username: asString(json['username']),
    nickname: asStringOrNull(json['nickname']),
    hasAvatar: json['has_avatar'] == true,
    avatarVersion: asIntOrNull(json['avatar_version']),
    purgeAfter: switch (asStringOrNull(json['purge_after'])) {
      final s? => DateTime.tryParse(s),
      _ => null,
    },
  );
}
