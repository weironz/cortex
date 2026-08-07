import 'dart:typed_data';

import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/memory_search_result.dart';
import '../models/session_detail.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';

/// Reports upload progress. [total] is 0 when the length is unknown.
typedef UploadProgress = void Function(int sent, int total);

/// The whole surface the UI is allowed to touch.
///
/// Two implementations exist — [HttpCortexApi] (real `cortexd`) and
/// [MockCortexApi] (in-memory fixtures). Which one is live is decided once, in
/// `cortexApiProvider`; no widget or controller knows the difference.
abstract interface class CortexApi {
  /// Human-readable name of the active backend, shown in the status strip.
  String get label;

  /// `GET /health`
  Future<HealthStatus> health();

  /// `POST /chat` → SSE.
  ///
  /// The returned stream is single-subscription and must be cancelled by the
  /// caller to abort an in-flight generation.
  ///
  /// [attachments] must already be **registered** blobs (via [uploadBlob] or
  /// presign+[commitBlob]); this call only associates them, it never carries
  /// bytes.
  ///
  /// There is deliberately no workspace parameter here. The sandbox root is a
  /// property of the *session*, set once through [updateSession]; sending it
  /// per message would put the fence's key on the same channel as the message
  /// the fence exists to contain.
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments,
  });

  /// `GET /memory/search?q=&limit=&as_of=`
  ///
  /// [asOf] replays the memory as it was *known* at that instant (transaction
  /// time), not as it was *true* — this is the "what did I believe three
  /// months ago" axis of the bitemporal model. Null means "now".
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  });

  /// `GET /episodes/{id}`
  Future<Episode> episode(String id);

  /// `GET /sessions?include_archived=`
  ///
  /// Archived sessions are omitted by default. Archiving is not deletion —
  /// episodes, attachments and extracted memory are all untouched — so the
  /// toggle is the only thing standing between the user and their history.
  Future<List<ChatSession>> sessions({bool includeArchived = false});

  /// `GET /sessions/{id}` — overview plus its episodes, oldest first.
  ///
  /// Capped server-side at 500 with no cursor, so a longer session arrives
  /// truncated *from the end*. Compare the episode count against
  /// `session.messageCount` to detect it — see [SessionDetail].
  Future<SessionDetail> sessionDetail(String id);

  /// `PATCH /sessions/{id}` — rename, archive, bind a workspace.
  ///
  /// The three fields are independent and all optional; only what is passed is
  /// changed, in one server-side write transaction.
  ///
  /// Workspace is tri-state and the two "no path" cases mean opposite things:
  ///
  /// | call | wire | meaning |
  /// |---|---|---|
  /// | neither arg | field absent | leave the binding alone |
  /// | `clearWorkspace: true` | `"workspace": null` | unbind, back to plain chat |
  /// | `workspace: "D:/x"` | `"workspace": "D:/x"` | bind |
  ///
  /// The daemon validates the path (absolute, exists, is a directory, not a
  /// filesystem root / system directory / the home directory itself, checked
  /// after symlink resolution) and its rejection message is written to be shown
  /// to the user verbatim — so callers should surface it, not replace it.
  ///
  /// Throws with [CortexApiException.isUnsupported] against a daemon that
  /// predates the route; callers keep the change local and flag it unsynced.
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  });

  /// `POST /blobs` — server-relayed upload, for content up to
  /// `kRelayUploadLimit`.
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/presign` — ask for a direct-to-object-store URL.
  ///
  /// Throws with `isUnsupported == true` on deployments backed by the local
  /// filesystem, which cannot sign URLs.
  Future<BlobPresign> presignBlob(String hash);

  /// `PUT` straight to the object store using a presigned URL. Not a cortexd
  /// endpoint — the whole point is that these bytes never touch the daemon.
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/commit` — register a directly-uploaded object.
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  });

  /// `GET /blobs/{hash}` — the bytes, relayed by the daemon.
  ///
  /// Deliberately bytes rather than a URL: the mock source has no origin to
  /// hand out, and going through one method keeps the attachment widgets from
  /// having to know which backend they are on.
  Future<Uint8List> blobBytes(String hash);

  /// `GET /ws` — **one** connection attempt.
  ///
  /// The returned stream ends when the socket closes and errors when it cannot
  /// be opened. Reconnection is deliberately *not* handled here: backoff,
  /// attempt counting and the cursor are policy, and policy belongs to
  /// `SyncController` where it can be unit-tested against a fake socket.
  Stream<SyncEvent> watchSync();

  /// `GET /sync?since=&limit=`
  ///
  /// [since] must be the caller's **own** cursor. Passing a cursor taken from a
  /// [SyncEvent] would skip everything between what we have and what the server
  /// reached — see the contract note on [SyncEvent].
  Future<SyncPage> sync({required int since, int limit = 500});

  void dispose();
}
