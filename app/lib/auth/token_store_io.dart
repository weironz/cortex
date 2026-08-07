import 'dart:io';

/// The environment variable `cortexd --generate-token` tells the operator to
/// set. Spelled the same way on purpose: a client that invented its own name
/// would make the daemon's own printed instructions wrong.
const String kTokenEnvVar = 'CORTEXD_TOKEN';

/// Desktop keeps nothing of its own — the environment already holds it.
///
/// False does **not** mean "you must retype the token every launch". It means
/// this app writes no copy: [readSeedToken] picks the value straight out of the
/// environment on every start, so setting the variable once is the persistence.
/// The UI says so rather than offering a checkbox that would only add a second,
/// worse copy of the same secret.
const bool kCanRememberToken = false;

/// Shown under the token field.
const String kTokenStorageNote =
    '桌面端从环境变量 $kTokenEnvVar 读取，不在本机留任何副本。'
    '想免去每次输入，把它设成用户级环境变量（PowerShell：'
    '`setx $kTokenEnvVar "<token>"`，之后重开应用生效）—— '
    '这正是 `cortexd --generate-token` 打印出来的那一行。';

/// Whatever the platform can supply without the user typing anything.
///
/// Read fresh rather than cached at startup: a token rotated while the app is
/// open should be picked up by the next login attempt, and the read costs
/// nothing.
String? readSeedToken() {
  final raw = Platform.environment[kTokenEnvVar]?.trim();
  return (raw == null || raw.isEmpty) ? null : raw;
}

/// No-op: there is nowhere to put it that is better than where it came from.
///
/// Present so the calling code has one shape on both platforms — the
/// alternative is a `kCanRememberToken` check at every call site, which is the
/// `kIsWeb` sprawl this seam exists to prevent.
Future<void> rememberToken(String token) async {}

Future<void> forgetToken() async {}
