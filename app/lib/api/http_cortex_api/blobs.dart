part of '../http_cortex_api.dart';

mixin _BlobsEndpoints on HttpCortexApiBase {
  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    final request = _ProgressRequest('POST', _uri('/blobs'), bytes, onProgress)
      ..headers.addAll(
        _headers({
          'accept': 'application/json',
          // The server sniffs the byte header and only falls back to this.
          'content-type': mime ?? 'application/octet-stream',
        }),
      );

    final json = await _sendJson(request, 'POST /blobs');
    return BlobRef.fromJson(json);
  }

  @override
  Future<BlobPresign> presignBlob(String hash) async =>
      BlobPresign.fromJson(await _postJson('/blobs/presign', {'hash': hash}));

  @override
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  }) async => BlobRef.fromJson(
    await _postJson('/blobs/commit', {
      'hash': hash,
      'size_bytes': sizeBytes,
      'mime': ?mime,
    }),
  );

  /// Straight to the object store. Deliberately **not** routed through
  /// [_uri] — the whole point of a presigned URL is that these bytes never
  /// reach cortexd.
  ///
  /// On Web this needs the bucket's own CORS policy to allow `PUT` from the
  /// page origin; cortexd's permissive CORS layer has no say over it.
  @override
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    final request = _ProgressRequest('PUT', Uri.parse(url), bytes, onProgress);
    if (mime != null) request.headers['content-type'] = mime;

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException('直传对象存储失败：$e', cause: e);
    }
    final body = await response.stream.bytesToString().catchError((_) => '');
    if (response.statusCode >= 400) {
      throw CortexApiException(
        body.isEmpty
            ? '直传返回 ${response.statusCode}'
            : HttpCortexApiBase._trim(body),
        statusCode: response.statusCode,
      );
    }
  }

  @override
  Future<Uint8List> blobBytes(String hash) async {
    final http.Response response;
    try {
      // A plain authenticated GET: this client renders attachments from bytes
      // it fetched itself, never from `Image.network`. That is why the ticket
      // is not needed here even though the daemon's docs list `<img>` among the
      // things it exists for — the widget layer already goes through
      // `CortexApi.blobBytes` so the mock source has something to answer with,
      // and the header path falls out of that for free.
      response = await _client.get(
        _uri('/blobs/${Uri.encodeComponent(hash)}'),
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
  Future<BlobUrl> blobUrl(String hash) async => BlobUrl.fromJson(
    await _getJson('/blobs/${Uri.encodeComponent(hash)}/url'),
  );
}
