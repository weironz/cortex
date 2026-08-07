import 'package:web/web.dart' as web;

/// Not read on Web — a browser tab has no environment. Kept so the two halves
/// of the seam expose the same names.
const String kTokenEnvVar = 'CORTEXD_TOKEN';

/// Web can persist, so the UI offers the choice — and states the cost.
const bool kCanRememberToken = true;

/// Key under which the opt-in copy lives.
///
/// Prefixed rather than bare `token` so a value belonging to this app is
/// recognisable in devtools, and so it cannot collide with anything else the
/// origin stores.
const String _kKey = 'cortex.token';

/// Shown next to the "remember" switch. It states the exposure plainly instead
/// of implying safety the browser cannot provide.
///
/// The distinction being drawn is real and worth the extra sentence: a user who
/// reads "记住" as "stored securely" will make a different deployment decision
/// (exposing cortexd to the internet) than one who reads it as "any script on
/// this page can read it".
const String kTokenStorageNote =
    '浏览器里没有安全的凭据存储：sessionStorage 与 localStorage 一样，'
    '会被本页面上运行的任何脚本读到。这里只用 sessionStorage —— '
    '它随标签页关闭而消失，刷新页面还在，但不会跨标签页、也不会留到明天。'
    '不勾选则只存在于内存中，刷新即失效。';

/// Reads the opt-in copy, if the user made one.
///
/// Storage access can throw outright — a sandboxed iframe or a browser
/// configured to block site data both raise on the *getter*, not on the value.
/// Treated as "nothing stored", because the alternative is a white screen at
/// launch caused by a privacy setting.
String? readSeedToken() {
  try {
    final raw = web.window.sessionStorage.getItem(_kKey)?.trim();
    return (raw == null || raw.isEmpty) ? null : raw;
  } on Object {
    return null;
  }
}

Future<void> rememberToken(String token) async {
  try {
    web.window.sessionStorage.setItem(_kKey, token);
  } on Object {
    // Storage blocked. The token still works for this run; silently keeping it
    // in memory is a better outcome than failing the login the user just
    // completed successfully.
  }
}

Future<void> forgetToken() async {
  try {
    web.window.sessionStorage.removeItem(_kKey);
  } on Object {
    // Nothing was stored if the setter failed earlier.
  }
}
