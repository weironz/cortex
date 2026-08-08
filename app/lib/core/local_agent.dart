/// The fourth platform seam: does this build get to run an agent of its own?
///
/// ## Why a seam, not a `kIsWeb` check
///
/// The Web build **cannot** spawn a process — `dart:io` does not exist there,
/// so an import of it fails at compile time, not at runtime. Guarding with
/// `kIsWeb` would not help: the import is what breaks.
///
/// ## Why the answer is genuinely different, not just "one platform lacks an API"
///
/// A browser tab has no filesystem to give an agent, and no way to hold a
/// long-lived child process. The Web client's honest shape is what it already
/// was: talk to the remote `cortexd` and let tools run there. Desktop's honest
/// shape is the opposite — your files are here, so the loop belongs here.
///
/// Both sides expose the same three names, so the calling code has one shape
/// and no widget checks `kIsWeb`.
library;

export 'local_agent_io.dart'
    if (dart.library.js_interop) 'local_agent_web.dart';
