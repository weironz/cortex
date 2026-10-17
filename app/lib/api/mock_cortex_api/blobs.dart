part of '../mock_cortex_api.dart';

mixin _BlobsMock on MockCortexApiBase {
  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    if (_disposed) throw const CortexApiException('Mock 数据源已关闭');
    if (bytes.length > kRelayUploadLimit) {
      // The daemon caps `POST /blobs` with `DefaultBodyLimit`; a mock that
      // accepted anything would let a bug in the routing logic pass unnoticed
      // right up until it hit a real server.
      throw CortexApiException(
        '请求体超过中转上限 ${formatBytes(kRelayUploadLimit)}',
        statusCode: 413,
      );
    }

    // Progress in a handful of steps over a realistic duration — an upload that
    // completes instantly never exercises the progress bar or the cancel path.
    const steps = 12;
    for (var i = 1; i <= steps; i++) {
      if (_disposed) throw const CortexApiException('Mock 数据源已关闭');
      await Future<void>.delayed(const Duration(milliseconds: 45));
      onProgress?.call((bytes.length * i / steps).round(), bytes.length);
    }

    final hash = await sha256Hex(bytes);
    final existed = _blobs.containsKey(hash);
    _blobs[hash] = bytes;
    return BlobRef(
      hash: hash,
      // Sniffed, not declared — same asymmetry as the server, so a `.txt` that
      // is really a PNG renders as an image here too.
      mime: _sniff(bytes) ?? mime ?? 'application/octet-stream',
      sizeBytes: bytes.length,
      deduplicated: existed,
      seq: existed ? null : _blobs.length,
    );
  }

  /// The local-filesystem blob backend cannot sign URLs and answers 501; the
  /// mock stands in for that deployment, which is the one this client is most
  /// likely to meet during development.
  @override
  Future<BlobPresign> presignBlob(String hash) async {
    await _latency(80);
    throw const CortexApiException(
      '对象存储后端 local_fs 签不出 presigned URL；'
      '请改用 POST /blobs 中转上传、GET /blobs/{hash} 取回',
      statusCode: 501,
    );
  }

  @override
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async => throw const CortexApiException('Mock 数据源不支持直传', statusCode: 501);

  @override
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  }) async => throw const CortexApiException('Mock 数据源不支持直传', statusCode: 501);

  @override
  Future<Uint8List> blobBytes(String hash) async {
    await _latency(70);
    final bytes = _blobs[hash];
    if (bytes == null) {
      throw CortexApiException('blob $hash 不存在', statusCode: 404);
    }
    return bytes;
  }

  @override
  Future<BlobUrl> blobUrl(String hash) async {
    await _latency(60);
    if (!_blobs.containsKey(hash)) {
      throw CortexApiException('blob $hash 不存在', statusCode: 404);
    }
    // 假后端没有对象存储，签不出真链接 —— 与「部署用本地文件系统」是
    // **同一种**失败（服务端那侧回 501）。回一条假链接的话，界面上
    // 「复制链接」看起来能用，粘出去是一个打不开的地址
    throw CortexApiException('这个后端没有对象存储，签不出直链', statusCode: 501);
  }

  /// Magic-number sniffing for the handful of types the UI branches on.
  static String? _sniff(Uint8List b) {
    bool starts(List<int> magic) {
      if (b.length < magic.length) return false;
      for (var i = 0; i < magic.length; i++) {
        if (b[i] != magic[i]) return false;
      }
      return true;
    }

    if (starts([0x89, 0x50, 0x4E, 0x47])) return 'image/png';
    if (starts([0xFF, 0xD8, 0xFF])) return 'image/jpeg';
    if (starts([0x47, 0x49, 0x46, 0x38])) return 'image/gif';
    if (starts([0x25, 0x50, 0x44, 0x46])) return 'application/pdf';
    if (starts([0x52, 0x49, 0x46, 0x46]) &&
        b.length > 11 &&
        b[8] == 0x57 &&
        b[9] == 0x45 &&
        b[10] == 0x42 &&
        b[11] == 0x50) {
      return 'image/webp';
    }
    return null;
  }

  // -------------------------------------------------------------------- chat
}
