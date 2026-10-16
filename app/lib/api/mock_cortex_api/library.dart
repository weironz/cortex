part of '../mock_cortex_api.dart';

mixin _LibraryMock on MockCortexApiBase {
  @override
  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
    String? sort,
  }) async {
    await _latency(70);
    var items = switch (folder) {
      null => _library,
      'none' => _library.where((i) => i.folderId == null).toList(),
      final f => _library.where((i) => i.folderId == f).toList(),
    };
    items = switch (tab) {
      'images' => items.where((i) => i.isImage).toList(),
      'files' => items.where((i) => !i.isImage).toList(),
      _ => items,
    };
    return LibraryPage(
      items: items.take(limit).toList(growable: false),
      folders: [
        for (final f in _folders)
          {
            'id': f.id,
            'name': f.name,
            'count': _library.where((i) => i.folderId == f.id).length,
          },
      ],
      hasMore: items.length > limit,
    );
  }

  @override
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  }) async {
    await _latency(70);
    // 同一份内容只有一条 —— 与服务端的 UNIQUE(blob_hash) 同一个语义。
    // mock 不照做的话，「拖两次同一个文件」在两个数据源上答得不一样
    final existing = _library.where((i) => i.blobHash == blobHash).firstOrNull;
    if (existing != null) return existing;
    final item = LibraryItem(
      id: 'MOCKLIB${_library.length}',
      blobHash: blobHash,
      name: name,
      mime: name.endsWith('.png') ? 'image/png' : 'text/markdown',
      sizeBytes: 4096,
      origin: origin ?? 'uploaded',
      folderId: folderId,
      chunkState: name.endsWith('.png')
          ? ChunkState.unsupported
          : ChunkState.ready,
      chunkCount: name.endsWith('.png') ? 0 : 3,
    );
    _library.insert(0, item);
    return item;
  }

  @override
  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  }) async {
    await _latency(60);
    final at = _library.indexWhere((i) => i.id == id);
    if (at < 0) throw const CortexApiException('没有这个条目', statusCode: 404);
    final old = _library[at];
    final next = LibraryItem(
      id: old.id,
      blobHash: old.blobHash,
      name: name ?? old.name,
      mime: old.mime,
      sizeBytes: old.sizeBytes,
      origin: old.origin,
      folderId: moveFolder ? folderId : old.folderId,
      chunkState: old.chunkState,
      chunkCount: old.chunkCount,
    );
    _library[at] = next;
    return next;
  }

  @override
  Future<void> removeFromLibrary(String id) async {
    await _latency(60);
    _library.removeWhere((i) => i.id == id);
  }

  @override
  Future<Folders> folders() async {
    await _latency(60);
    return Folders(
      folders: [
        for (final f in _folders)
          Folder(
            id: f.id,
            name: f.name,
            count: _imageFolder.values.where((v) => v == f.id).length,
            coverHash: _gallery
                .where((i) => _imageFolder[i.id] == f.id)
                .map((i) => i.hash)
                .firstOrNull,
          ),
      ],
    );
  }

  @override
  Future<Folders> createFolder(String name) async {
    await _latency(60);
    _folders.insert(0, Folder(id: 'MOCKFOLDER${_folders.length}', name: name));
    return folders();
  }

  @override
  Future<Folders> renameFolder(String id, String name) async {
    await _latency(60);
    final at = _folders.indexWhere((f) => f.id == id);
    if (at >= 0) _folders[at] = Folder(id: id, name: name);
    return folders();
  }

  @override
  Future<Folders> deleteFolder(String id) async {
    await _latency(60);
    _folders.removeWhere((f) => f.id == id);
    // 里面的东西回未归档 —— 与服务端的 ON DELETE SET NULL 同一个语义。
    // mock 这边不照做的话，「删文件夹会不会删掉里面的图」在两个数据源上
    // 答得不一样，而界面是照 mock 调出来的
    _imageFolder.removeWhere((_, folder) => folder == id);
    return folders();
  }
}
