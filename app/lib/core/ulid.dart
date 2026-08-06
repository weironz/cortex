import 'dart:math';

/// Minimal ULID generator.
///
/// Cortex uses ULIDs as primary keys precisely so that any client can mint an
/// id without coordinating with the server — that is what lets the UI create a
/// session and start streaming into it before `cortexd` has ever heard of it.
/// Rolling our own (~30 lines) beats a dependency for that.
///
/// Layout: 10 chars of millisecond timestamp + 16 chars of randomness, encoded
/// in Crockford base32 (no I, L, O, U).
abstract final class Ulid {
  static const _alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  static final _random = Random.secure();

  static String generate() {
    final buffer = StringBuffer();

    var time = DateTime.now().millisecondsSinceEpoch;
    final timeChars = List<String>.filled(10, '0');
    for (var i = 9; i >= 0; i--) {
      timeChars[i] = _alphabet[time & 0x1F];
      time >>= 5;
    }
    buffer.writeAll(timeChars);

    for (var i = 0; i < 16; i++) {
      buffer.write(_alphabet[_random.nextInt(32)]);
    }
    return buffer.toString();
  }
}
