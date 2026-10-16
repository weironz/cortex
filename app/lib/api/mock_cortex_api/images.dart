part of '../mock_cortex_api.dart';

mixin _ImagesMock on MockCortexApiBase {
  @override
  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  }) async {
    // 真后端上这一步要几十秒。**假的也要慢一点** —— 秒回的话
    // 「生成中」那个状态在 demo 里根本看不见，而它恰恰是这一页最需要
    // 做对的一段（用户要等半分钟，界面必须说清它在干活）
    await _latency(900);
    final hashes = <String>[];
    for (var i = 0; i < n.clamp(1, 4); i++) {
      final bytes = _swatch(_gallery.length + i);
      final hash = await sha256Hex(bytes);
      _blobs[hash] = bytes;
      hashes.add(hash);
      _gallery.insert(
        0,
        GeneratedImage(
          // 画廊按 id 倒序，而假 id 也要保证**新的更大** ——
          // 否则 demo 里新画的图会排到最后，看起来像没生成
          id: 'MOCKIMG${(999999 - _gallery.length).toString().padLeft(8, '0')}',
          hash: hash,
          prompt: prompt,
          model: model ?? 'qwen-image-3.0',
          source: source ?? 'mock-source',
          size: size,
          createdAt: DateTime.now(),
        ),
      );
    }
    return hashes;
  }

  @override
  Future<String> shareImage(String id) async {
    await _latency(60);
    _shared[id] = 'https://example.invalid/s/mocktoken/image.png';
    return _shared[id]!;
  }

  @override
  Future<void> unshareImage(String id) async {
    await _latency(60);
    _shared.remove(id);
  }

  @override
  Future<void> removeImage(String id) async {
    await _latency(60);
    _gallery.removeWhere((i) => i.id == id);
    _shared.remove(id);
    _imageFolder.remove(id);
  }

  @override
  Future<void> moveImage(String id, String? folderId) async {
    await _latency(60);
    if (folderId == null) {
      _imageFolder.remove(id);
    } else {
      _imageFolder[id] = folderId;
    }
  }

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) async {
    await _latency(80);
    var items = switch (folder) {
      null => _gallery,
      'none' => _gallery.where((i) => !_imageFolder.containsKey(i.id)).toList(),
      final f => _gallery.where((i) => _imageFolder[i.id] == f).toList(),
    };
    if (hash != null) {
      items = items.where((i) => i.hash == hash).toList();
    }
    if (before != null) {
      final at = items.indexWhere((i) => i.id == before);
      items = at < 0 ? const [] : items.sublist(at + 1);
    }
    final page = items.take(limit).toList(growable: false);
    final more = items.length > limit;
    return Gallery(
      items: page,
      hasMore: more,
      nextCursor: more ? page.last.id : null,
    );
  }

  /// 一张纯色小 PNG —— 让画廊在假后端上也画得出东西。
  ///
  /// **真的是合法 PNG 字节**，不是占位符：`Image.memory` 解不开的话，
  /// 那一格会画成「破图」图标，而那正是这一页最需要被测到的样子。
  static Uint8List _swatch(int i) {
    // 1×1 的 PNG，调色板里换一个颜色就是另一张图（内容寻址要求字节不同，
    // 否则几张图会去重成同一个 hash，画廊上看起来像只生成了一张）
    const palette = [0xE8, 0x9A, 0x5C, 0x3F, 0x77, 0xC4];
    final rgb = [
      palette[i % palette.length],
      palette[(i + 2) % palette.length],
      palette[(i + 4) % palette.length],
    ];
    final ihdr = _png([
      0, 0, 0, 1, // width 1
      0, 0, 0, 1, // height 1
      8, 2, 0, 0, 0, // bit depth 8, colour type 2 (truecolour)
    ], 'IHDR');
    // zlib：无压缩 deflate 块，内容是「过滤器字节 0 + RGB」
    final raw = [0, ...rgb];
    final adler = _adler32(raw);
    final idat = _png([
      0x78, 0x01, // zlib header
      0x01, // final, stored block
      raw.length, 0x00, // len
      0xFF - raw.length, 0xFF, // ~len
      ...raw,
      (adler >> 24) & 0xFF, (adler >> 16) & 0xFF,
      (adler >> 8) & 0xFF, adler & 0xFF,
    ], 'IDAT');
    return Uint8List.fromList([
      0x89,
      0x50,
      0x4E,
      0x47,
      0x0D,
      0x0A,
      0x1A,
      0x0A,
      ...ihdr,
      ...idat,
      ..._png(const [], 'IEND'),
    ]);
  }

  /// 一个 PNG 块：长度 + 类型 + 数据 + CRC32。
  static List<int> _png(List<int> data, String type) {
    final tag = type.codeUnits;
    final body = [...tag, ...data];
    final crc = _crc32(body);
    return [
      (data.length >> 24) & 0xFF,
      (data.length >> 16) & 0xFF,
      (data.length >> 8) & 0xFF,
      data.length & 0xFF,
      ...body,
      (crc >> 24) & 0xFF,
      (crc >> 16) & 0xFF,
      (crc >> 8) & 0xFF,
      crc & 0xFF,
    ];
  }

  static int _crc32(List<int> bytes) {
    var crc = 0xFFFFFFFF;
    for (final b in bytes) {
      crc ^= b;
      for (var k = 0; k < 8; k++) {
        crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xEDB88320 : crc >> 1;
      }
    }
    return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF;
  }

  static int _adler32(List<int> bytes) {
    var a = 1, b = 0;
    for (final byte in bytes) {
      a = (a + byte) % 65521;
      b = (b + a) % 65521;
    }
    return ((b << 16) | a) & 0xFFFFFFFF;
  }
}
