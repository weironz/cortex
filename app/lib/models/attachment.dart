import 'json.dart';

/// A blob attached to one message.
///
/// ## Only two of these fields exist on the wire
///
/// `cortexd`'s `AttachmentRef` is `{ hash, kind }` and nothing else. Everything
/// below that — [filename], [mime], [sizeBytes] — is **client-side enrichment**
/// that survives only as long as the app is running: it is known while the user
/// is uploading, and lost the moment the transcript is replayed from
/// `GET /sessions/{id}`.
///
/// That is a real contract gap, not an oversight here. A replayed image renders
/// fine (the bytes are addressed by [hash]), but a replayed PDF can only be
/// labelled "文档 · a1b2c3d4…" because the original name was never stored.
/// Fixing it means adding `filename` / `mime` / `size_bytes` to `AttachmentRef`
/// server-side; until then the UI degrades honestly rather than inventing a
/// name.
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
  /// Null on a replayed attachment.
  final String? mime;

  /// Null on a replayed attachment.
  final int? sizeBytes;

  /// Local only — never sent, never returned. See the class doc.
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

  /// Wire shape for `ChatRequest.attachments`. Only the two fields the server
  /// accepts — sending more would be silently dropped and give a false
  /// impression that filenames round-trip.
  Map<String, dynamic> toWireJson() => {
    'hash': hash,
    if (kind != null) 'kind': kind,
  };

  factory Attachment.fromJson(Map<String, dynamic> json) => Attachment(
    hash: asString(json['hash']),
    kind: asStringOrNull(json['kind']),
    // Tolerated in case the server later grows these; harmless when absent.
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
