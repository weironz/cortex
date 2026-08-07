import 'json.dart';

/// A registered binary object (`POST /blobs`, `POST /blobs/commit`).
class BlobRef {
  const BlobRef({
    required this.hash,
    required this.mime,
    required this.sizeBytes,
    this.deduplicated = false,
    this.seq,
  });

  /// SHA-256, lowercase hex — content identity and retrieval key.
  final String hash;

  /// Sniffed from the byte header by the server, **not** what we declared.
  final String mime;
  final int sizeBytes;

  /// The content was already registered. Observability only — idempotency is
  /// guaranteed by content addressing, not by this flag.
  final bool deduplicated;

  /// Position in the sync total order; null when no row was added.
  final int? seq;

  factory BlobRef.fromJson(Map<String, dynamic> json) => BlobRef(
    hash: asString(json['hash']),
    mime: asString(json['mime'], 'application/octet-stream'),
    sizeBytes: asInt(json['size_bytes']),
    deduplicated: json['deduplicated'] == true,
    seq: json['seq'] == null ? null : asInt(json['seq']),
  );
}

/// Answer to `POST /blobs/presign`.
class BlobPresign {
  const BlobPresign({
    required this.url,
    required this.method,
    required this.expiresInSecs,
    required this.alreadyUploaded,
  });

  final String url;

  /// Always `PUT` today; read from the response so a future switch to a POST
  /// form does not need a client change.
  final String method;
  final int expiresInSecs;

  /// The object store already holds these bytes — skip the upload and go
  /// straight to `/blobs/commit`. This is the bandwidth saving content
  /// addressing buys: forwarding the same photo twice uploads once.
  final bool alreadyUploaded;

  factory BlobPresign.fromJson(Map<String, dynamic> json) => BlobPresign(
    url: asString(json['url']),
    method: asString(json['method'], 'PUT'),
    expiresInSecs: asInt(json['expires_in_secs'], 900),
    alreadyUploaded: json['already_uploaded'] == true,
  );
}
