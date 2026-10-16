part of '../http_cortex_api.dart';

mixin _ImagesEndpoints on HttpCortexApiBase {
  @override
  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  }) async {
    final body = await _postJson('/llm/image', {
      'prompt': prompt,
      // 三个都用 `?`：null 就整个键不出现，而服务端那侧「键不出现」
      // 正是「你替我挑」—— 传 null 进去会被当成一个显式的空值
      'model': ?model,
      'source': ?source,
      'size': ?size,
      'n': n,
      // `session_id` 刻意**不传**：从图片页画的图不属于任何一条会话，
      // 画廊里那一列为 NULL 就是这个意思。对话里画的由 agent 那条路带
    });
    return [
      for (final m in (body['images'] as List? ?? const []))
        if (m is Map && m['hash'] is String) m['hash'] as String,
    ];
  }

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) async => Gallery.fromJson(
    await _getJson('/images', {
      'limit': '$limit',
      'before': ?before,
      'folder': ?folder,
      'hash': ?hash,
    }),
  );

  @override
  Future<String> shareImage(String id) async => asString(
    (await _postJson('/images/${Uri.encodeComponent(id)}/share', null))['url'],
  );

  @override
  Future<void> unshareImage(String id) =>
      _noContent('DELETE', '/images/${Uri.encodeComponent(id)}/share');

  @override
  Future<void> removeImage(String id) =>
      _noContent('DELETE', '/images/${Uri.encodeComponent(id)}');

  @override
  Future<void> moveImage(String id, String? folderId) => _noContent(
    'PATCH',
    '/images/${Uri.encodeComponent(id)}',
    // 显式带 null 而不是省掉这个键：省掉的话服务端读成「没说要动归属」，
    // 于是「移出文件夹」这个动作静默地什么都不做
    {'folder_id': folderId},
  );
}
