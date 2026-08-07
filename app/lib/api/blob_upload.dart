import 'dart:typed_data';

import '../core/hashing.dart';
import '../models/attachment.dart';
import '../models/blob.dart';
import 'api_exception.dart';
import 'cortex_api.dart';

/// The size at which a client should stop relaying through the daemon.
///
/// **32 MiB, copied from `cortexd::blobs::DIRECT_UPLOAD_LIMIT`** — this is not a
/// number picked here. The daemon caps `POST /blobs` at exactly this value, so
/// choosing anything higher would produce a 413 that no amount of client-side
/// cleverness can fix, and choosing much lower would push small files onto the
/// three-round-trip presign path for no reason.
///
/// The daemon's own reasoning, which this client simply inherits: relaying means
/// the same bytes travel twice (client → cortexd → object store), which is a
/// doubled uplink bill on the exact files where uplink hurts. Below the
/// threshold the two extra round trips of presign cost more than the doubled
/// transfer saves.
///
/// If the two ever drift, the daemon wins and the client sees a 413 — so this
/// constant is worth re-checking whenever `blobs.rs` changes.
const int kRelayUploadLimit = 32 * 1024 * 1024;

/// Uploads [bytes] and returns the registered [Attachment].
///
/// Picks the route by size, and degrades in the one direction that can work:
///
/// * `<= kRelayUploadLimit` → `POST /blobs`. One round trip, and the server
///   sniffs the MIME from the byte header, so nothing has to be guessed.
/// * `> kRelayUploadLimit` → hash locally, `POST /blobs/presign`, `PUT` to the
///   object store, `POST /blobs/commit`. If the store already has the content
///   the upload is skipped entirely — the payoff of content addressing.
///
/// A deployment backed by the local filesystem cannot sign URLs and answers
/// 501. There is no fallback for that case: the relay route is *already* known
/// to be too small for this file, so retrying it would only trade a clear
/// error for a 413. The message says what to change instead.
Future<Attachment> uploadAttachment(
  CortexApi api, {
  required Uint8List bytes,
  required String filename,
  String? mime,
  UploadProgress? onProgress,
}) async {
  final declared = mime ?? mimeFromFilename(filename);

  if (bytes.length <= kRelayUploadLimit) {
    final blob = await api.uploadBlob(
      bytes: bytes,
      mime: declared,
      onProgress: onProgress,
    );
    return _toAttachment(blob, filename);
  }

  final hash = await sha256Hex(bytes);

  final BlobPresign presign;
  try {
    presign = await api.presignBlob(hash);
  } on CortexApiException catch (e) {
    if (e.isUnsupported) {
      throw CortexApiException(
        '这份文件有 ${formatBytes(bytes.length)}，超过中转上限 '
        '${formatBytes(kRelayUploadLimit)}，只能直传对象存储；'
        '但当前部署的存储后端签不出直传 URL。请配置 S3 后端，或换一个小一些的文件。',
        statusCode: e.statusCode,
        cause: e.cause,
      );
    }
    rethrow;
  }

  if (presign.alreadyUploaded) {
    // Same bytes are already in the store. Report the skip as instant
    // completion so the progress bar does not sit at 0% and then vanish.
    onProgress?.call(bytes.length, bytes.length);
  } else {
    await api.putPresigned(
      url: presign.url,
      bytes: bytes,
      mime: declared,
      onProgress: onProgress,
    );
  }

  final blob = await api.commitBlob(
    hash: hash,
    sizeBytes: bytes.length,
    mime: declared,
  );
  return _toAttachment(blob, filename);
}

Attachment _toAttachment(BlobRef blob, String filename) => Attachment(
  hash: blob.hash,
  // Bucket from the **sniffed** MIME, not the declared one: a `.txt` that is
  // really a PNG should render as an image.
  kind: kindFromMime(blob.mime),
  mime: blob.mime,
  sizeBytes: blob.sizeBytes,
  filename: filename,
);
