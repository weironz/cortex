part of '../cortex_api.dart';

mixin CortexApiSync {
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
}
