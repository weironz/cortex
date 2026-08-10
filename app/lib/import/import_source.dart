library;

import 'dart:typed_data';

/// The fourth platform seam, and the one with the largest number attached to it.
///
/// ## Why the two platforms genuinely differ
///
/// A Claude export is **97 MB**. What happens to those bytes is not a styling
/// choice:
///
/// * **Desktop** runs a local agent on the same machine as the file. Handing it
///   a *path* means the bytes are read once, by the process that parses them,
///   and never cross a socket. The daemon on the other side of the network
///   never sees them at all.
/// * **Web** has no such process. A browser tab cannot read a path, so the only
///   way to parse the file is to send it — which is why `cortexd` grew
///   `POST /import/upload` (streamed to a spool file, never buffered whole).
///
/// So the seam is not "one platform lacks an API". It is "on one platform the
/// bytes stay put, on the other they must travel", and pretending otherwise
/// would mean uploading 97 MB from a machine that did not need to.
///
/// ## Why the picked file is modelled as a sealed type
///
/// The caller has to make a real decision — upload or don't — and a
/// `String? path` plus a `Uint8List? bytes` would let it forget. With a sealed
/// type the switch is exhaustive and the compiler notices.
export 'import_source_io.dart'
    if (dart.library.js_interop) 'import_source_web.dart';

/// A chosen export file, in whichever form this platform can supply.
sealed class ImportSource {
  const ImportSource({required this.filename, required this.sizeBytes});

  /// Shown in the UI so the user can confirm they picked the right file.
  final String filename;

  /// Shown next to the name. On Web it is also what tells the user how much is
  /// about to be uploaded — 97 MB deserves to be said out loud before it moves.
  final int sizeBytes;
}

/// The file is on this machine and a local agent can read it.
final class ImportPath extends ImportSource {
  const ImportPath({
    required this.path,
    required super.filename,
    required super.sizeBytes,
  });

  final String path;
}

/// The file had to be read into memory, and therefore has to be uploaded.
final class ImportBytes extends ImportSource {
  const ImportBytes({
    required this.bytes,
    required super.filename,
    required super.sizeBytes,
  });

  final Uint8List bytes;
}
