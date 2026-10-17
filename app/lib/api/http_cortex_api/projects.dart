part of '../http_cortex_api.dart';

mixin _ProjectsEndpoints on HttpCortexApiBase {
  @override
  Future<List<Project>> projects() async {
    final json = await _getJson('/projects');
    return asObjectList(
      json['projects'],
    ).map(Project.fromJson).toList(growable: false);
  }

  @override
  Future<Project> createProject(String name) async =>
      Project.fromJson(await _postJson('/projects', {'name': name}));

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) async =>
      Project.fromJson(
        await _patchJson('/projects/${Uri.encodeComponent(id)}', {
          'name': ?name,
          'pinned': ?pinned,
        }),
      );

  @override
  Future<void> deleteProject(String id) async {
    final http.Response response;
    try {
      response = await _client.delete(
        _uri('/projects/${Uri.encodeComponent(id)}'),
        headers: _headers(const {'accept': 'application/json'}),
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
    // 回的是 `{}`，没有可读的东西 —— 不解析，省得哪天服务端改成 204
    // 就要在这里炸一次
  }
}
