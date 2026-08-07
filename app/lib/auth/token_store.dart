/// The third — and, like the other two, deliberately narrow — platform seam.
///
/// ## Why a seam was unavoidable
///
/// The token is a **bearer credential**: whoever holds it owns the entire
/// memory store (see `cortexd::auth`). Where it is allowed to rest is therefore
/// a security question, and the honest answer is genuinely different on the two
/// platforms — not merely "one has an API the other lacks":
///
/// * **Desktop** has an ambient, per-user, non-git-tracked place to keep a
///   secret that this process can read and a web page cannot: the environment.
///   `cortexd --generate-token` already prints `CORTEXD_TOKEN=…` for exactly
///   this, so honouring it is following the contract rather than inventing one.
/// * **Web** has no environment. Everything a browser tab can persist —
///   `localStorage`, `sessionStorage`, IndexedDB, cookies readable by JS — is
///   readable by *any* script running on the same origin. There is no
///   browser-side storage that meaningfully protects this value, so the choice
///   is only about **how long the exposure lasts**.
///
/// ## What is deliberately not done
///
/// * **No `--dart-define=CORTEX_TOKEN`.** A compile-time constant is baked into
///   the artifact. On Web that artifact is `main.dart.js`, served to anyone who
///   loads the page — the token would ship to every visitor. Convenient and
///   catastrophic, so the door is not opened at all, not even for desktop
///   (a build flag that works on one platform and leaks on the other is a trap
///   waiting for whoever runs `flutter build web` next).
/// * **No `localStorage` on Web.** It persists until explicitly cleared and is
///   shared by every tab on the origin. `sessionStorage` has the identical XSS
///   exposure but dies with the tab, which bounds the damage in time; when
///   neither is actually safe, the one with the shorter life wins.
/// * **No plaintext file on desktop.** Without an OS keychain binding it would
///   be strictly worse than the environment variable: same exposure, plus a
///   file that outlives the user's memory of having created it.
///
/// Everything above this file asks [kCanRememberToken] once and renders
/// accordingly. No widget imports `dart:io`, no widget touches `package:web`,
/// and no widget checks `kIsWeb`.
library;

export 'token_store_io.dart'
    if (dart.library.js_interop) 'token_store_web.dart';
