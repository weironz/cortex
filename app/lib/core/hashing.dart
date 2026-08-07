import 'dart:math' as math;
import 'dart:typed_data';

import 'package:crypto/crypto.dart';

/// Lowercase-hex SHA-256 of [bytes], computed in chunks.
///
/// ## Why chunked, and why async
///
/// This is only ever called on the presign path, i.e. on files large enough
/// that the server declined to relay them (> 32 MiB). Hashing 200 MB in one
/// synchronous call blocks the UI isolate for long enough to drop hundreds of
/// frames, and the progress bar the user is watching would freeze precisely
/// while the "slow" part happens.
///
/// Chunking with an `await` between blocks yields to the event loop, so the
/// frame pipeline keeps running. It is not free — the hash takes marginally
/// longer in wall-clock terms — but a responsive window during a 30-second
/// upload is worth more than the milliseconds.
///
/// An isolate would be strictly better on desktop. It is not used because
/// `dart:isolate` does not exist on Web, and this is the one place where a
/// platform fork would have to leak out of `api/http_client_factory*.dart` —
/// the app's single deliberate platform seam. Chunking is the portable answer
/// that keeps that invariant.
Future<String> sha256Hex(Uint8List bytes) async {
  const chunk = 1 << 20; // 1 MiB
  final sink = _DigestSink();
  final converter = sha256.startChunkedConversion(sink);
  for (var i = 0; i < bytes.length; i += chunk) {
    final end = math.min(i + chunk, bytes.length);
    converter.add(Uint8List.sublistView(bytes, i, end));
    await Future<void>.delayed(Duration.zero);
  }
  converter.close();
  return sink.value!.toString();
}

/// Captures the single digest `startChunkedConversion` emits on close.
///
/// `package:convert`'s `AccumulatorSink` would do the same, but pulling in a
/// dependency for four lines is not a trade worth making.
class _DigestSink implements Sink<Digest> {
  Digest? value;

  @override
  void add(Digest data) => value = data;

  @override
  void close() {}
}
