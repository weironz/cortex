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
