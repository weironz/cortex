/// 资料库里的一条 —— agent 随时能取的材料，与某一条对话无关。
///
/// 与 [`GeneratedImage`] 的分工：那个是**画出来的产物**（图库），
/// 这个是**你自己的材料**（上传的文档、以及收进来的图）。两者共用
/// 同一份文件夹清单（见 `Folders`）。
library;

import 'json.dart';

/// 正文切分到哪一步了。
enum ChunkState {
  /// 还没切。
  pending,

  /// 切好了，能被检索到。
  ready,

  /// **这类文件现在提不出正文**（pdf / docx / xlsx，以及图片）。
  ///
  /// ⚠️ 这不是失败：提取器还没做。界面上要说「这类文件还提不出正文」，
  /// 不能说「切分失败」—— 后者会让人去重试一件重试一百次也不会成的事。
  unsupported,

  /// 真的切坏了（读出来不是合法 UTF-8 之类）。
  failed;

  static ChunkState fromWire(String? s) =>
      ChunkState.values.where((v) => v.name == s).firstOrNull ??
      ChunkState.pending;

  /// 界面上那一行小字。
  String get label => switch (this) {
    ChunkState.pending => '待切分',
    ChunkState.ready => '',
    ChunkState.unsupported => '这类文件还提不出正文',
    ChunkState.failed => '正文没提出来',
  };
}

class LibraryItem {
  const LibraryItem({
    required this.id,
    required this.blobHash,
    required this.name,
    required this.mime,
    this.sizeBytes = 0,
    this.origin = 'uploaded',
    this.folderId,
    this.chunkState = ChunkState.pending,
    this.chunkCount = 0,
  });

  factory LibraryItem.fromJson(Map<String, dynamic> json) => LibraryItem(
    id: asString(json['id']),
    blobHash: asString(json['blob_hash']),
    name: asString(json['name']),
    mime: asString(json['mime']),
    sizeBytes: asInt(json['size_bytes']),
    origin: asString(json['origin']),
    folderId: json['folder_id'] as String?,
    chunkState: ChunkState.fromWire(json['chunk_state'] as String?),
    chunkCount: asInt(json['chunk_count']),
  );

  final String id;
  final String blobHash;
  final String name;
  final String mime;
  final int sizeBytes;

  /// `uploaded`（你传的）/ `generated`（agent 画的，被收进来了）。
  final String origin;

  /// 归到哪个文件夹。`null` = 未归档。
  final String? folderId;

  final ChunkState chunkState;
  final int chunkCount;

  bool get isImage => mime.startsWith('image/');

  /// 卡片上那行小字：大小 · 段数 / 或者「提不出正文」那句。
  String get subtitle {
    final size = _humanSize(sizeBytes);
    final note = chunkState.label;
    if (note.isNotEmpty) return '$size · $note';
    if (chunkCount > 0) return '$size · $chunkCount 段';
    return size;
  }
}

String _humanSize(int bytes) {
  if (bytes >= 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  if (bytes >= 1024) return '${(bytes / 1024).round()} KB';
  return '$bytes B';
}

/// `GET /library` 的一页。
/// 资料库占了多少、上限多少。
///
/// # 为什么它必须画出来
///
/// 附件与 agent 交付的产物是**自动**收进资料库的，判据是「不判断值不值得收」
/// （见 `docs/library-content.md`）。那个决定成立的前提是**膨胀有办法管** ——
/// 而办法就是配额加上「看得见、删得掉」。不画出来的话，用户第一次知道有上限
/// 是在它满了、东西收不进去的那一刻。
class LibraryUsage {
  const LibraryUsage({this.bytes = 0, this.quotaBytes = 0, this.items = 0});

  factory LibraryUsage.fromJson(Map<String, dynamic> json) => LibraryUsage(
    bytes: asInt(json['bytes']),
    quotaBytes: asInt(json['quota_bytes']),
    items: asInt(json['items']),
  );

  final int bytes;

  /// `0` = 不限（本机开发默认这么设）。
  final int quotaBytes;
  final int items;

  /// 有没有上限这回事。没有就不画那条进度条 —— 一条永远填不满的条子
  /// 只会让人猜它是什么意思。
  bool get hasQuota => quotaBytes > 0;

  /// 用掉的比例，0..1。没有上限时回 0。
  double get ratio =>
      hasQuota ? (bytes / quotaBytes).clamp(0.0, 1.0).toDouble() : 0;
}

class LibraryPage {
  const LibraryPage({
    this.items = const [],
    this.folders = const [],
    this.hasMore = false,
    this.usage = const LibraryUsage(),
  });

  factory LibraryPage.fromJson(Map<String, dynamic> json) => LibraryPage(
    items: asObjectList(
      json['items'],
    ).map(LibraryItem.fromJson).toList(growable: false),
    folders: asObjectList(json['folders']),
    hasMore: json['has_more'] == true,
    // 老服务端不发这一位 —— 那时 `hasQuota` 为假，界面不画那条进度条，
    // 而不是画一条 0/0
    usage: LibraryUsage.fromJson(
      json['usage'] is Map<String, dynamic>
          ? json['usage'] as Map<String, dynamic>
          : const {},
    ),
  );

  /// 占了多少、上限多少。**跟着列表一起回** —— 分成两条路的话，打开资料库
  /// 要两次往返，而其中一条失败时那一行会空着，用户不知道那是第二个请求。
  final LibraryUsage usage;

  final List<LibraryItem> items;

  /// 文件夹清单**只在第一页给**（翻页时它们不会变，每页带一遍是白费带宽）。
  /// 原样是 JSON —— 解析成 `Folder` 的活儿交给 `Folders` 那个模型，
  /// 免得同一个形状在两处各解一遍。
  final List<Map<String, dynamic>> folders;

  final bool hasMore;
}
