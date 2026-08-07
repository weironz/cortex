import 'dart:typed_data';

import 'package:cross_file/cross_file.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/blob_upload.dart';
import '../core/ulid.dart';
import '../models/attachment.dart';
import 'app_providers.dart';

enum UploadStatus { uploading, ready, failed }

/// One file on its way from the user's disk into `episode_blobs`.
///
/// The bytes are held for the whole lifetime of the entry, not just during the
/// upload, because a failed upload has to be retryable without asking the user
/// to find the file again — and on Web there is no path to re-open it from.
class PendingAttachment {
  const PendingAttachment({
    required this.id,
    required this.filename,
    required this.bytes,
    required this.mime,
    this.status = UploadStatus.uploading,
    this.sent = 0,
    this.attachment,
    this.error,
  });

  /// Client-local, so the list has stable keys while entries come and go.
  final String id;
  final String filename;
  final Uint8List bytes;
  final String? mime;
  final UploadStatus status;

  /// Bytes handed to the transport so far. See `_ProgressRequest` for what that
  /// means on Web versus desktop.
  final int sent;

  /// Set once the blob is registered; this is what actually gets sent.
  final Attachment? attachment;
  final String? error;

  int get total => bytes.length;

  double get progress => total == 0 ? 1 : (sent / total).clamp(0, 1);

  bool get isImage => (mime ?? '').startsWith('image/');

  PendingAttachment copyWith({
    UploadStatus? status,
    int? sent,
    Attachment? attachment,
    Object? error = _sentinel,
  }) => PendingAttachment(
    id: id,
    filename: filename,
    bytes: bytes,
    mime: mime,
    status: status ?? this.status,
    sent: sent ?? this.sent,
    attachment: attachment ?? this.attachment,
    error: error == _sentinel ? this.error : error as String?,
  );

  static const Object _sentinel = Object();
}

/// The composer's attachment tray, keyed by session.
///
/// Keyed rather than global because switching sessions mid-draft must not carry
/// one conversation's files into another's prompt — the two are as separate as
/// their text drafts would be.
class AttachmentQueue extends Notifier<Map<String, List<PendingAttachment>>> {
  @override
  Map<String, List<PendingAttachment>> build() {
    // A data-source swap invalidates every blob hash we hold: the mock's store
    // and the daemon's store share no content. Keeping the tray would let a
    // send reference a hash the new backend has never registered.
    ref.listen(cortexApiProvider, (_, _) => state = const {});
    return const {};
  }

  List<PendingAttachment> forSession(String? sessionId) =>
      sessionId == null ? const [] : (state[sessionId] ?? const []);

  /// Registered blobs ready to ride along with the next message.
  List<Attachment> readyFor(String? sessionId) => [
    for (final p in forSession(sessionId)) ?p.attachment,
  ];

  bool hasPending(String? sessionId) =>
      forSession(sessionId).any((p) => p.status == UploadStatus.uploading);

  /// Opens the platform file chooser and uploads whatever comes back.
  ///
  /// `withData: true` is required on desktop (where the default is a path
  /// only); on Web it is already the default. The bytes are needed either way —
  /// content addressing means the hash has to be computed over them, and there
  /// is no path to hand the daemon.
  Future<void> pickAndUpload(String sessionId) async {
    final result = await FilePicker.pickFiles(
      allowMultiple: true,
      withData: true,
      dialogTitle: '选择要附加的文件',
    );
    if (result == null) return;
    for (final file in result.files) {
      final bytes = file.bytes;
      if (bytes == null) continue;
      await addBytes(
        sessionId,
        filename: file.name,
        bytes: bytes,
        mime: mimeFromFilename(file.name),
      );
    }
  }

  /// Drag-and-drop entry point. `XFile` is what both `desktop_drop` and
  /// `file_picker` speak, so the two paths converge here.
  Future<void> addDropped(String sessionId, List<XFile> files) async {
    for (final file in files) {
      final Uint8List bytes;
      try {
        bytes = await file.readAsBytes();
      } on Object catch (e) {
        // A dropped *directory* lands here on desktop: it has a path but
        // cannot be read as a file. Reported per-item so the rest of the drop
        // still goes through.
        _put(
          sessionId,
          PendingAttachment(
            id: Ulid.generate(),
            filename: file.name,
            bytes: Uint8List(0),
            mime: null,
            status: UploadStatus.failed,
            error: '读不出内容（拖进来的可能是个目录）：$e',
          ),
        );
        continue;
      }
      await addBytes(
        sessionId,
        filename: file.name,
        bytes: bytes,
        mime: file.mimeType ?? mimeFromFilename(file.name),
      );
    }
  }

  Future<void> addBytes(
    String sessionId, {
    required String filename,
    required Uint8List bytes,
    String? mime,
  }) async {
    final entry = PendingAttachment(
      id: Ulid.generate(),
      filename: filename,
      bytes: bytes,
      mime: mime,
    );
    _put(sessionId, entry);
    await _upload(sessionId, entry.id);
  }

  Future<void> retry(String sessionId, String id) async {
    final entry = _find(sessionId, id);
    if (entry == null || entry.bytes.isEmpty) return;
    _update(
      sessionId,
      id,
      (p) => p.copyWith(status: UploadStatus.uploading, sent: 0, error: null),
    );
    await _upload(sessionId, id);
  }

  Future<void> _upload(String sessionId, String id) async {
    final entry = _find(sessionId, id);
    if (entry == null) return;
    try {
      final attachment = await uploadAttachment(
        ref.read(cortexApiProvider),
        bytes: entry.bytes,
        filename: entry.filename,
        mime: entry.mime,
        // The entry may have been removed by the user mid-upload; `_update` is
        // a no-op then, so no state is resurrected.
        onProgress: (sent, _) => _update(sessionId, id, (p) => p.copyWith(sent: sent)),
      );
      _update(
        sessionId,
        id,
        (p) => p.copyWith(
          status: UploadStatus.ready,
          sent: p.total,
          attachment: attachment,
        ),
      );
    } on CortexApiException catch (e) {
      _update(
        sessionId,
        id,
        (p) => p.copyWith(status: UploadStatus.failed, error: e.message),
      );
    } on Object catch (e) {
      _update(
        sessionId,
        id,
        (p) => p.copyWith(status: UploadStatus.failed, error: '$e'),
      );
    }
  }

  void remove(String sessionId, String id) {
    final list = state[sessionId];
    if (list == null) return;
    state = {
      ...state,
      sessionId: list.where((p) => p.id != id).toList(growable: false),
    };
  }

  /// Called after a successful send. The blobs stay registered server-side —
  /// they are referenced by the episode now — so only the tray is cleared.
  void clear(String sessionId) {
    if (!state.containsKey(sessionId)) return;
    final next = {...state}..remove(sessionId);
    state = next;
  }

  PendingAttachment? _find(String sessionId, String id) {
    for (final p in state[sessionId] ?? const <PendingAttachment>[]) {
      if (p.id == id) return p;
    }
    return null;
  }

  void _put(String sessionId, PendingAttachment entry) {
    state = {
      ...state,
      sessionId: [...?state[sessionId], entry],
    };
  }

  void _update(
    String sessionId,
    String id,
    PendingAttachment Function(PendingAttachment) update,
  ) {
    if (!ref.mounted) return;
    final list = state[sessionId];
    if (list == null) return;
    final index = list.indexWhere((p) => p.id == id);
    if (index == -1) return;
    final next = [...list];
    next[index] = update(next[index]);
    state = {...state, sessionId: next};
  }
}

final attachmentQueueProvider =
    NotifierProvider<AttachmentQueue, Map<String, List<PendingAttachment>>>(
      AttachmentQueue.new,
    );
