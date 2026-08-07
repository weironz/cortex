import 'json.dart';

/// A blob attached to one message.
///
/// ## The two directions carry different fields
///
/// * **Up** (`AttachmentRef`, in `POST /chat`) is `{ hash, kind, filename }`.
///   [mime] and [sizeBytes] are deliberately *not* sent: they are decided by
///   the bytes, which the server already sniffed when the blob was registered.
///   Reporting them again would only create a case where what the client says
///   and what the bytes say disagree.
/// * **Down** (`AttachmentDto`, on every replayed episode) adds [mime] and
///   [sizeBytes] to those three.
///
/// [filename] therefore round-trips now. It lives on `episode_blobs` rather
/// than on `blobs` because under content addressing one set of bytes can have
/// many names — the same screenshot is "设计稿.png" today and "IMG_2043.png"
/// when someone forwards it — so the name belongs to *this reference*, not to
/// the content.
///
/// A null [filename] on a replayed attachment means the row predates that
/// column; [displayName] falls back to "文档 · a1b2c3d4" for those rather than
/// inventing a name.
class Attachment {
  const Attachment({
    required this.hash,
    this.kind,
    this.mime,
    this.sizeBytes,
    this.filename,
  });

  /// SHA-256, lowercase hex. Both the content identity and the path parameter
  /// for `GET /blobs/{hash}`.
  final String hash;

  /// Semantic bucket: `image` / `audio` / `video` / `document`. Deliberately a
  /// free string — `episode_blobs.kind` does not enumerate, so neither do we.
  final String? kind;

  /// Sniffed by the server from the byte header (not what the client claimed).
  final String? mime;

  final int? sizeBytes;

  /// The name the user saw when they attached it. Sent up, stored per
  /// reference, and returned on replay — see the class doc.
  final String? filename;

  bool get isImage =>
      kind == 'image' || (mime != null && mime!.startsWith('image/'));

  /// What to show when there is no [filename]: the kind plus a short hash, so
  /// two different attachments are still visibly different.
  String get displayName {
    if (filename != null && filename!.isNotEmpty) return filename!;
    final head = hash.length > 8 ? hash.substring(0, 8) : hash;
    return '${kindLabel(kind)} · $head';
  }

  Attachment copyWith({String? kind, String? mime, int? sizeBytes}) =>
      Attachment(
        hash: hash,
        kind: kind ?? this.kind,
        mime: mime ?? this.mime,
        sizeBytes: sizeBytes ?? this.sizeBytes,
        filename: filename,
      );

  /// Wire shape for `ChatRequest.attachments` — the three fields `AttachmentRef`
  /// accepts. [mime] and [sizeBytes] are omitted on purpose; the server derives
  /// them from the bytes it already holds.
  Map<String, dynamic> toWireJson() => {
    'hash': hash,
    if (kind != null) 'kind': kind,
    if (filename != null && filename!.isNotEmpty) 'filename': filename,
  };

  factory Attachment.fromJson(Map<String, dynamic> json) => Attachment(
    hash: asString(json['hash']),
    kind: asStringOrNull(json['kind']),
    mime: asStringOrNull(json['mime']),
    sizeBytes: json['size_bytes'] == null ? null : asInt(json['size_bytes']),
    filename: asStringOrNull(json['filename']),
  );
}

/// Maps a MIME type onto the `kind` vocabulary the server stores.
String kindFromMime(String? mime) {
  final m = (mime ?? '').toLowerCase();
  if (m.startsWith('image/')) return 'image';
  if (m.startsWith('audio/')) return 'audio';
  if (m.startsWith('video/')) return 'video';
  return 'document';
}

String kindLabel(String? kind) => switch (kind) {
  'image' => '图片',
  'audio' => '音频',
  'video' => '视频',
  'document' => '文档',
  null => '附件',
  final other => other,
};

/// Best-effort MIME from a filename extension.
///
/// Only a hint: the server sniffs the byte header and overrides this whenever
/// it can. It exists so that a `.png` picked on a platform that reports no MIME
/// still gets bucketed as an image in the composer preview.
String? mimeFromFilename(String name) {
  final dot = name.lastIndexOf('.');
  if (dot < 0 || dot == name.length - 1) return null;
  return switch (name.substring(dot + 1).toLowerCase()) {
    'png' => 'image/png',
    'jpg' || 'jpeg' => 'image/jpeg',
    'gif' => 'image/gif',
    'webp' => 'image/webp',
    'bmp' => 'image/bmp',
    'svg' => 'image/svg+xml',
    'pdf' => 'application/pdf',
    'txt' || 'log' => 'text/plain',
    'md' => 'text/markdown',
    'json' => 'application/json',
    'csv' => 'text/csv',
    'zip' => 'application/zip',
    'mp3' => 'audio/mpeg',
    'wav' => 'audio/wav',
    'm4a' => 'audio/mp4',
    'mp4' => 'video/mp4',
    'mov' => 'video/quicktime',
    'webm' => 'video/webm',
    _ => null,
  };
}

/// `1.4 MB`, `823 KB`, `512 B`.
String formatBytes(int? bytes) {
  if (bytes == null) return '';
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(0)} KB';
  if (bytes < 1024 * 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} GB';
}
