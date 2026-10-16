part of '../http_cortex_api.dart';

mixin _LibraryEndpoints on HttpCortexApiBase {
  @override
  Future<Folders> folders() async =>
      Folders.fromJson(await _getJson('/folders'));

  @override
  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
    String? sort,
  }) async => LibraryPage.fromJson(
    await _getJson('/library', {
      'limit': '$limit',
      'before': ?before,
      'folder': ?folder,
      'tab': ?tab,
      'sort': ?sort,
    }),
  );

  @override
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  }) async => LibraryItem.fromJson(
    await _postJson('/library', {
      'blob_hash': blobHash,
      'name': name,
      'origin': ?origin,
      'folder_id': ?folderId,
    }),
  );

  @override
  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  }) async => LibraryItem.fromJson(
    await _patchJson('/library/${Uri.encodeComponent(id)}', {
      'name': ?name,
      // ⚠️ 只有真要动归属时才带这个键。带一个 null 与不带是两回事：
      // 服务端把「带了 null」读成「移出文件夹」，而一次只改名字的请求
      // 顺手把归档清掉，用户看到的是文件自己从文件夹里跑出来了
      if (moveFolder) 'folder_id': folderId,
    }),
  );

  @override
  Future<void> removeFromLibrary(String id) =>
      _noContent('DELETE', '/library/${Uri.encodeComponent(id)}');

  @override
  Future<Folders> createFolder(String name) async =>
      Folders.fromJson(await _postJson('/folders', {'name': name}));

  @override
  Future<Folders> renameFolder(String id, String name) async =>
      Folders.fromJson(
        await _patchJson('/folders/${Uri.encodeComponent(id)}', {'name': name}),
      );

  @override
  Future<Folders> deleteFolder(String id) async => Folders.fromJson(
    await _deleteJson('/folders/${Uri.encodeComponent(id)}'),
  );
}
