part of '../http_cortex_api.dart';

mixin _SandboxEndpoints on HttpCortexApiBase {
  @override
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId}) =>
      _sandboxBytes('/sandbox/workspace.tar', _scoped(null, sessionId));

  @override
  Future<Uint8List> sandboxReadFile(String path, {String? sessionId}) =>
      _sandboxBytes('/sandbox/files/raw', _scoped(path, sessionId));

  @override
  Future<List<FileNode>> sandboxListFiles(
    String path, {
    String? sessionId,
  }) async {
    final json = await _getJson('/sandbox/files', _scoped(path, sessionId));
    // 子节点的绝对路径拿**服务端回的** path 去拼，不是请求里那个：服务端会
    // 规范化（消掉多余的斜杠、结尾的 `/`），用请求里那份拼出来的路径，
    // 下一次展开送回去的就是一条服务端没见过的字符串
    final dir = asString(json['path'], path);
    return asObjectList(json['entries'])
        .map((e) {
          final name = asString(e['name']);
          final isDir = e['is_dir'] == true;
          return FileNode(
            name: name,
            path: posixJoin(dir, name),
            isDirectory: isDir,
            // 目录的 size 服务端固定给 0，那不是「空目录」的意思 —— 传 null，
            // 界面因此不会在目录行后面画一个 0 B
            sizeBytes: isDir ? null : asInt(e['size']),
            // Unix 秒。服务端给 null 表示 tar 里没有这个字段 ——
            // **不要**回落到 0，那会显示成 1970 年
            modifiedAt: switch (e['mtime']) {
              final int s when s > 0 => DateTime.fromMillisecondsSinceEpoch(
                s * 1000,
                isUtc: true,
              ).toLocal(),
              _ => null,
            },
          );
        })
        .toList(growable: false);
  }

  @override
  Future<SandboxWriteReceipt> sandboxWriteFile({
    required String path,
    required Uint8List bytes,
    UploadProgress? onProgress,
    String? sessionId,
  }) async {
    // 复用 `uploadBlob` 那套分块请求：一个 `http.Request` 只有 0% 与 done
    // 两个状态，而工作区里塞进来的常常是几十 MB 的数据集。
    // Web 上进度的语义见 `_ProgressRequest` 的文档（会先冲到 100% 再等）
    final request =
        _ProgressRequest(
            'PUT',
            // 路径在 query，字节在 body。反过来（多段表单）要额外一层编码，
            // 而这条路由两边都只认原始字节
            _uri('/sandbox/files', _scoped(path, sessionId)),
            bytes,
            onProgress,
          )
          ..headers.addAll(
            _headers(const {
              'content-type': 'application/octet-stream',
              'accept': 'application/json',
            }),
          );

    final http.Response response;
    try {
      response = await http.Response.fromStream(await _client.send(request));
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
    final json = _decodeObject(
      'PUT /sandbox/files',
      utf8.decode(response.bodyBytes),
    );
    return (path: asString(json['path'], path), size: asInt(json['size']));
  }

  // ----------------------------------------------------------------- plumbing
}
