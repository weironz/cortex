part of '../http_cortex_api.dart';

mixin _AuthEndpoints on HttpCortexApiBase {
  @override
  Future<AuthTokens> login(String username, String password) =>
      _postAuth('/auth/login', {'username': username, 'password': password});

  @override
  Future<AuthTokens> register(String username, String password) =>
      _postAuth('/auth/register', {'username': username, 'password': password});

  @override
  Future<AuthTokens> refreshSession(String refreshToken) =>
      _postAuth('/auth/refresh', {'refresh_token': refreshToken});

  @override
  Future<Account?> whoAmI() async {
    try {
      return Account.fromJson(await _getJson('/auth/me'));
    } on CortexApiException catch (e) {
      // 老服务端没有这个端点、或者这个部署压根没有账号体系。
      // 两种都只意味着「没有名字可显示」—— 账号栏照常在，
      // 它还挂着设置与退出登录。
      if (e.isMissing || e.statusCode == 501) return null;
      rethrow;
    }
  }

  @override
  Future<Profile> profile() async =>
      Profile.fromJson(await _getJson('/auth/profile'));

  @override
  Future<Profile> updateProfile({Patch<String?>? nickname}) async {
    // **只放进这次真的要改的字段。** 字段缺席 = 服务端不动它；
    // 无脑把所有字段都发出去的话，一次「只改昵称」会把别的字段一起重写成
    // 界面上那份可能已经过期的副本
    final body = <String, dynamic>{
      if (nickname != null) 'nickname': nickname.value,
    };
    return Profile.fromJson(await _patchJson('/auth/profile', body));
  }

  @override
  Future<Profile> putAvatar(Uint8List bytes) async {
    // body 是原始字节，不是 multipart：这条路只上传一个文件、没有别的字段，
    // multipart 会多一层双方都要维护的解析
    final req = http.Request('PUT', _uri('/auth/avatar'))
      ..headers.addAll(_headers({'content-type': 'application/octet-stream'}))
      ..bodyBytes = bytes;
    return Profile.fromJson(await _sendJson(req, '上传头像'));
  }

  @override
  Future<Profile> deleteAvatar() async =>
      Profile.fromJson(await _deleteJson('/auth/avatar'));

  @override
  Future<Uint8List> avatarBytes(String userId) async {
    // 与 `blobBytes` 同一条路：自己取字节、自己渲染。不用 `Image.network`
    // ——它带不了 Authorization，而 mock 数据源也答不出一个 URL
    final http.Response response;
    try {
      response = await _client.get(
        _uri('/auth/avatar/${Uri.encodeComponent(userId)}'),
        headers: _headers(),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        HttpCortexApiBase._trim(response.body),
        headers: response.headers,
      );
    }
    return response.bodyBytes;
  }

  @override
  Future<void> startEmailBinding(String email) async {
    await _postJson('/auth/email/start', {'email': email});
  }

  @override
  Future<Profile> verifyEmail(String code) async =>
      Profile.fromJson(await _postJson('/auth/email/verify', {'code': code}));

  @override
  Future<Profile> unbindEmail(String password) async {
    // DELETE 带 body —— `_deleteJson` 不带，走 `_sendJson`
    final req = http.Request('DELETE', _uri('/auth/email'))
      ..headers.addAll(_headers({'content-type': 'application/json'}))
      ..body = jsonEncode({'password': password});
    return Profile.fromJson(await _sendJson(req, '解绑邮箱'));
  }

  @override
  Future<void> changePassword(String oldPassword, String newPassword) async {
    await _postJson('/auth/password', {
      'old_password': oldPassword,
      'new_password': newPassword,
    });
  }

  @override
  Future<Profile> deleteAccount(String password) async {
    // DELETE 带 body —— `_deleteJson` 不带，所以走 `_sendJson`
    final req = http.Request('DELETE', _uri('/auth/account'))
      ..headers.addAll(_headers({'content-type': 'application/json'}))
      ..body = jsonEncode({'password': password});
    return Profile.fromJson(await _sendJson(req, '删除账号'));
  }

  @override
  Future<Profile> restoreAccount() async =>
      Profile.fromJson(await _postJson('/auth/account/restore', const {}));

  @override
  Future<void> logout(String refreshToken) async {
    try {
      await _client.post(
        _uri('/auth/logout'),
        headers: _headers(const {'content-type': 'application/json'}),
        body: jsonEncode({'refresh_token': refreshToken}),
      );
    } on Object catch (_) {
      // 登出失败不该拦住用户离开。本地那份已经会被清掉，而服务端那条链
      // 最多再活到过期 —— 把人卡在「退不出去」的界面上更糟
    }
  }

  /// 三条账号端点形状一样：POST JSON，回 AuthTokens。
  ///
  /// **不带 Authorization 头**：它们在服务端是免认证的公开端点
  /// （登录时本来就还没有凭据），带一个过期的 access token 反而会让
  /// 请求被中间层拦下。
  Future<AuthTokens> _postAuth(String path, Map<String, Object?> body) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri(path),
        headers: const {
          'content-type': 'application/json',
          'accept': 'application/json',
        },
        body: jsonEncode(body),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return AuthTokens.fromJson(
      _decodeObject(path, utf8.decode(response.bodyBytes)),
    );
  }

  @override
  Future<AuthTicket> issueTicket() async {
    // **No request body**, not even `{}`.
    //
    // The handler takes no input, so axum never reads the body; hyper then
    // closes the connection instead of returning it to the keep-alive pool.
    // `IOClient` does not learn of that and hands the dead socket to the next
    // request, which fails with a bare "Write failed" — observed as the *second*
    // ticket request dying while the first succeeded. Sending nothing keeps the
    // connection reusable, which matters here precisely because this endpoint is
    // called again on every reconnect.
    final json = await _postJson('/auth/ticket', null);
    return AuthTicket(
      value: asString(json['ticket']),
      expiresAt: DateTime.now().add(
        Duration(seconds: asInt(json['expires_in_secs'], 60)),
      ),
    );
  }
}
