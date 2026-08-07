import 'dart:typed_data';

import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/memory_search_result.dart';
import '../models/pending_confirmation.dart';
import '../models/session_detail.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';

/// Reports upload progress. [total] is 0 when the length is unknown.
typedef UploadProgress = void Function(int sent, int total);

/// A short-lived credential that is allowed to travel in a URL.
class AuthTicket {
  const AuthTicket({required this.value, required this.expiresAt});

  final String value;

  /// Local wall-clock expiry, computed from the server's `expires_in_secs` at
  /// the moment of receipt — same reasoning as
  /// [PendingConfirmation.deadline]: a duration needs no agreement between
  /// clocks, a timestamp does.
  final DateTime expiresAt;

  /// True with a margin, because a ticket that expires mid-handshake produces a
  /// 401 the reconnect loop then has to interpret. Sixty seconds is short
  /// enough that spending some of it on safety costs nothing — the ticket is
  /// re-minted with one cheap POST.
  bool isUsableAt(DateTime now) =>
      expiresAt.difference(now) > const Duration(seconds: 10);
}

/// The whole surface the UI is allowed to touch.
///
/// Two implementations exist — [HttpCortexApi] (real `cortexd`) and
/// [MockCortexApi] (in-memory fixtures). Which one is live is decided once, in
/// `cortexApiProvider`; no widget or controller knows the difference.
abstract interface class CortexApi {
  /// Human-readable name of the active backend, shown in the status strip.
  String get label;

  /// `GET /health` — the **only** unauthenticated route.
  ///
  /// That is what makes it usable as the login gate's probe: it answers
  /// `auth: "token" | "disabled"` to a client that has no credential yet, which
  /// is the one question such a client needs answered before it can decide
  /// whether to demand one from the user.
  Future<HealthStatus> health();

  /// `POST /auth/ticket` — trade the long-lived token for a 60-second one.
  ///
  /// Exists because `WebSocket` (and `EventSource`, and `<img src>`) cannot
  /// carry an `Authorization` header in a browser — a hard API limitation, not
  /// something to engineer around. The ticket, and only the ticket, is allowed
  /// into a URL: query strings reach access logs, reverse-proxy logs and
  /// browser history, and a credential that expires in a minute survives that
  /// far better than one that never expires.
  ///
  /// Authenticated itself, so it doubles as the cheapest possible "is this
  /// token accepted?" probe — see `AuthController.signIn`.
  Future<AuthTicket> issueTicket();

  /// `GET /confirmations?session_id=` — what is still waiting for an answer.
  ///
  /// This is the **reconnect** path. `POST /chat`'s SSE stream is single-shot
  /// with no `Last-Event-ID` replay, so a confirmation request that was in
  /// flight when the socket dropped is never re-sent; without this call the
  /// turn would sit suspended until it timed out, with nothing on screen to
  /// explain why. It is also how a *second* device discovers that the first one
  /// was asked something.
  Future<List<PendingConfirmation>> pendingConfirmations({String? sessionId});

  /// `POST /confirmations` — deliver the user's answer.
  ///
  /// Returns false when the daemon answers 404, which is **not** a failure: the
  /// token is one-shot and four ordinary situations consume it — another device
  /// answered first, the 180-second timeout fired, the turn ended, or the
  /// daemon restarted. The server refuses to distinguish them (telling them
  /// apart would let someone probe for "is a command being approved right
  /// now?"), so neither can the client. Callers should retire the prompt
  /// quietly, not raise an error.
  ///
  /// [allow] false sends `deny`, which is a *different* thing from letting the
  /// clock run out even though the agent treats both as refusal: a denial
  /// arrives immediately and frees the suspended turn, rather than parking a
  /// database connection and a model context for three minutes.
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
  });

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

  /// `GET /sessions/{id}?limit=&before=` — overview plus **one page** of
  /// episodes, oldest first within the page.
  ///
  /// With no [before] the page is the *newest* one. To walk backwards, pass the
  /// previous response's `nextCursor`; stop when `hasMore` is false. [limit] is
  /// clamped server-side to 500.
  ///
  /// The cursor is opaque — constructing one locally earns a 400, which is the
  /// point: it is validated before it reaches SQL.
  Future<SessionDetail> sessionDetail(String id, {int? limit, String? before});

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
