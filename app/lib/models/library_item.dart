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
class LibraryPage {
  const LibraryPage({
    this.items = const [],
    this.folders = const [],
    this.hasMore = false,
  });

  factory LibraryPage.fromJson(Map<String, dynamic> json) => LibraryPage(
    items: asObjectList(
      json['items'],
    ).map(LibraryItem.fromJson).toList(growable: false),
    folders: asObjectList(json['folders']),
    hasMore: json['has_more'] == true,
  );

  final List<LibraryItem> items;

  /// 文件夹清单**只在第一页给**（翻页时它们不会变，每页带一遍是白费带宽）。
  /// 原样是 JSON —— 解析成 `Folder` 的活儿交给 `Folders` 那个模型，
  /// 免得同一个形状在两处各解一遍。
  final List<Map<String, dynamic>> folders;

  final bool hasMore;
}
