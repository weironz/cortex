/// The second — and last — platform seam in the app.
///
/// ## Why one was unavoidable here
///
/// The first seam (`api/http_client_factory.dart`) exists because SSE needs a
/// different transport on Web. This one exists for a blunter reason: **Web has
/// no filesystem.** A browser tab cannot enumerate `D:/codes/cortex`, and it
/// cannot be given a native directory picker.
///
/// Note what this seam is *not* about: it is not "the workspace feature is
/// desktop-only". The binding itself works everywhere, because a workspace is a
/// path on **cortexd's** machine and the daemon validates it (see
/// `cortexd::workspace::validate`). What Web loses is the two conveniences that
/// need local disk access — the native folder dialog, and a locally-rendered
/// file tree.
///
/// Everything above this file asks [kCanBrowseLocalFiles] once and renders
/// accordingly. No widget imports `dart:io`, and no widget checks `kIsWeb`.
library;

export 'workspace_fs_io.dart'
    if (dart.library.js_interop) 'workspace_fs_web.dart';
