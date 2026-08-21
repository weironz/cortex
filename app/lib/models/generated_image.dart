/// 画廊里的一张图 —— `GET /images` 的一项。
///
/// # 为什么带着提示词
///
/// 「以此为提示词重画」是这一页唯一的再生产动作，而它要的就是那句话。
/// 让客户端去 episode 里翻的话，从图片页直接画的那些**根本没有 episode**。
library;

import 'json.dart';

class GeneratedImage {
  const GeneratedImage({
    required this.id,
    required this.hash,
    required this.prompt,
    required this.model,
    required this.source,
    this.size,
    this.sessionId,
    this.createdAt,
  });

  factory GeneratedImage.fromJson(Map<String, dynamic> json) => GeneratedImage(
    id: asString(json['id']),
    hash: asString(json['hash']),
    prompt: asString(json['prompt']),
    model: asString(json['model']),
    source: asString(json['source']),
    size: json['size'] as String?,
    sessionId: json['session_id'] as String?,
    createdAt: DateTime.tryParse(asString(json['created_at']))?.toLocal(),
  );

  final String id;

  /// blob 哈希。取图与附件同一条路（`CortexApi.blobBytes`）。
  final String hash;
  final String prompt;

  /// **实际用的**那个，不是请求里写的那个 —— 请求可以不指名，由服务端挑。
  final String model;
  final String source;

  /// `宽*高`。`null` = 当时没指定尺寸。
  final String? size;

  /// 在哪条会话里画的。`null` = 从图片页直接画的。
  final String? sessionId;

  /// 解析不出来时是 `null`。**不拿「现在」顶替** ——
  /// 一个编出来的时间会让整面墙的排序看起来是对的，而它其实是乱的。
  final DateTime? createdAt;
}

/// `GET /images` 的一页。
class Gallery {
  const Gallery({this.items = const [], this.hasMore = false, this.nextCursor});

  factory Gallery.fromJson(Map<String, dynamic> json) => Gallery(
    items: asObjectList(
      json['items'],
    ).map(GeneratedImage.fromJson).toList(growable: false),
    hasMore: json['has_more'] == true,
    nextCursor: json['next_cursor'] as String?,
  );

  final List<GeneratedImage> items;

  /// 还有更早的吗。**不靠「条数 == limit」去猜** ——
  /// 恰好整除时那个猜法会多翻一页空的，界面上是「加载中…」闪一下。
  final bool hasMore;
  final String? nextCursor;
}

/// `GET /blobs/{hash}/url` —— 一条**会过期**的直链。
///
/// 过期这件事必须一路带到界面上：用户复制一条链接是为了发给别人，
/// 而一条十五分钟后 404 的链接，比「复制不了」更坏 —— 他不会再回来看，
/// 只会以为是对方打不开。
class BlobUrl {
  const BlobUrl({required this.url, required this.expiresInSecs});

  factory BlobUrl.fromJson(Map<String, dynamic> json) => BlobUrl(
    url: asString(json['url']),
    expiresInSecs: asInt(json['expires_in_secs']),
  );

  final String url;
  final int expiresInSecs;
}
