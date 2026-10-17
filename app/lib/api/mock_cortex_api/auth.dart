part of '../mock_cortex_api.dart';

mixin _AuthMock on MockCortexApiBase {
  /// The mock stands in for a plain cortexd: no local agent, so no local
  /// workspace route. Reporting it as unsupported is what makes the caller
  /// exercise its `PATCH /sessions/{id}` fallback in tests.
  /// The mock has no file to parse, so import is reported as unavailable
  /// rather than faked. A fake bill would be the one number in this screen
  /// that must never be invented — it is what the user approves spending.
  @override
  Future<AuthTokens> login(String username, String password) async {
    throw const CortexApiException('这个数据源不支持账号登录', statusCode: 501);
  }

  /// Mock 没有账号。返回 null 而不是编一个名字：账号栏会照实说
  /// 「Mock 数据源」，而一个假名字会让人以为自己连着真后端。
  @override
  Future<Account?> whoAmI() async => null;

  // ── 账号资料 ──
  //
  // mock 里存**内存中的一份可变状态**而不是一律 501：账号页要能在没有
  // 服务端时打开、改昵称、看到改完的结果 —— 那正是这个数据源存在的意义
  // （widget 测试与「先看看长什么样」都靠它）。
  //

  // 删号那两条例外，一律 501：它们在真部署上是不可逆的，mock 里假装成功
  // 会让人以为验过了那条路，而那是最不该被假装的一条。
  Profile _profile = const Profile(
    userId: 'mock-user',
    username: 'demo',
    nickname: '演示账号',
  );

  @override
  Future<Profile> profile() async => _profile;

  /// 只给测试用：把资料摆成某个状态（比如「已排期删除」）。
  ///
  /// 走一个显式命名的方法而不是把 `_profile` 开成 public：后者会让产品
  /// 代码也能改它，而那时「mock 的资料从哪来」就有两个答案了。
  @visibleForTesting
  void setProfileForTest(Profile p) => _profile = p;

  @override
  Future<Profile> updateProfile({Patch<String?>? nickname}) async {
    if (nickname != null) {
      final v = nickname.value?.trim();
      _profile = Profile(
        userId: _profile.userId,
        username: _profile.username,
        nickname: (v == null || v.isEmpty) ? null : v,
        hasAvatar: _profile.hasAvatar,
        avatarVersion: _profile.avatarVersion,
        purgeAfter: _profile.purgeAfter,
      );
    }
    return _profile;
  }

  @override
  Future<Profile> putAvatar(Uint8List bytes) async {
    _avatar = bytes;
    _profile = Profile(
      userId: _profile.userId,
      username: _profile.username,
      nickname: _profile.nickname,
      hasAvatar: true,
      avatarVersion: DateTime.now().millisecondsSinceEpoch ~/ 1000,
      purgeAfter: _profile.purgeAfter,
    );
    return _profile;
  }

  @override
  Future<Profile> deleteAvatar() async {
    _avatar = null;
    _profile = Profile(
      userId: _profile.userId,
      username: _profile.username,
      nickname: _profile.nickname,
      purgeAfter: _profile.purgeAfter,
    );
    return _profile;
  }

  Uint8List? _avatar;

  @override
  Future<Uint8List> avatarBytes(String userId) async {
    final bytes = _avatar;
    if (bytes == null) {
      throw const CortexApiException('这个账号没有设头像', statusCode: 404);
    }
    return bytes;
  }

  // 邮箱那三条一律 501：mock 里没有发信通道，假装成功会让人以为验过了
  // 那条路 —— 而绑定邮箱恰恰是「只有真发出去才算数」的那一类
  @override
  Future<void> startEmailBinding(String email) async {
    throw const CortexApiException('这个数据源不支持发信', statusCode: 501);
  }

  @override
  Future<Profile> verifyEmail(String code) async {
    throw const CortexApiException('这个数据源不支持发信', statusCode: 501);
  }

  @override
  Future<Profile> unbindEmail(String password) async {
    throw const CortexApiException('这个数据源不支持发信', statusCode: 501);
  }

  @override
  Future<void> changePassword(String oldPassword, String newPassword) async {
    throw const CortexApiException('这个数据源不支持改口令', statusCode: 501);
  }

  @override
  Future<Profile> deleteAccount(String password) async {
    throw const CortexApiException('这个数据源不支持删除账号', statusCode: 501);
  }

  @override
  Future<Profile> restoreAccount() async {
    throw const CortexApiException('这个数据源不支持删除账号', statusCode: 501);
  }

  @override
  Future<AuthTokens> register(String username, String password) async {
    throw const CortexApiException('这个数据源不支持账号注册', statusCode: 501);
  }

  @override
  Future<AuthTokens> refreshSession(String refreshToken) async {
    throw const CortexApiException('这个数据源不支持账号登录', statusCode: 501);
  }

  @override
  Future<void> logout(String refreshToken) async {}

  @override
  Future<AuthTicket> issueTicket() async {
    await _latency(40);
    return AuthTicket(
      value: 'mock-ticket',
      expiresAt: DateTime.now().add(const Duration(seconds: 60)),
    );
  }
}
