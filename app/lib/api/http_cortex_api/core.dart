part of '../http_cortex_api.dart';

mixin _CoreEndpoints on HttpCortexApiBase {
  @override
  String get label => '$_base · $kHttpClientKind';

  @override
  Future<HealthStatus> health() async =>
      HealthStatus.fromJson(await _getJson('/health'));

  /// `GET /sandbox/health`。**不抛** —— 见接口上那段文档。
  ///
  /// 刻意不走 [_getJson]：那条路把每个 >= 400 都翻成异常，而这里
  /// 「404」与「502」恰恰是两个**要显示出来的不同答案**。
  @override
  Future<SandboxHealth> sandboxHealth() async {
    final http.Response response;
    try {
      response = await _client.get(
        _uri('/sandbox/health'),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      // 连不上这个地址本身。`/health` 会同时红，而那一条说得清楚得多 ——
      // 这里只说「问不出来」，不再复述一遍「连不上 cortexd」
      return SandboxHealth.blocked('探测不到 /sandbox/health：$e');
    }

    // 404/405 = 这个地址上没有 agent 编排服务。**不是故障**：自托管与
    // 纯本机部署本来就没有它。
    //
    // 401 也走这里而不是敲 [onUnauthorized] 的门：这条路由在服务端是
    // 公开的，收到 401 只说明前面挡着一层我们不认识的东西 —— 拿它去
    // 判定「凭据过期」会把用户从一次健康探测里踢出登录态。
    if (response.statusCode == 404 ||
        response.statusCode == 405 ||
        response.statusCode == 401) {
      return SandboxHealth.absent;
    }
    if (response.statusCode >= 400) {
      // 502/503/504 是网关在说「我认得这条路，但后面那个进程没起来」。
      // 那与「压根没有沙箱」是两件事，一起吞成 absent 会让一次真正的
      // 宕机看起来像一个正常的自托管形态
      return SandboxHealth.blocked(_errorMessage(response));
    }

    final Object? decoded;
    try {
      decoded = jsonDecode(utf8.decode(response.bodyBytes));
    } on FormatException {
      // 200 + 一张网页：nginx 对认不出的路径做 SPA 回落。CLAUDE.md 专门
      // 记了这个假信号 —— 「回 200 + index.html，看起来像成功」
      return SandboxHealth.absent;
    }
    if (decoded is! Map<String, dynamic>) return SandboxHealth.absent;
    return SandboxHealth.fromJson(decoded);
  }
}
