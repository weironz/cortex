library;

import 'dart:io';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// The environment variable `cortexd --generate-token` tells the operator to
/// set. Spelled the same way on purpose: a client that invented its own name
/// would make the daemon's own printed instructions wrong.
const String kTokenEnvVar = 'CORTEXD_TOKEN';

/// Desktop **does** keep a copy now — in the OS credential store.
///
/// ## This file used to say the opposite, and the reason changed
///
/// The old position was "no copy at all, read `CORTEXD_TOKEN` from the
/// environment every launch", and the argument was sound *for what existed
/// then*: the only alternative on the table was a plaintext file, which is
/// strictly worse than an environment variable — same exposure, plus a file
/// that outlives the user's memory of having created it.
///
/// What changed is not our appetite for risk; it is **what is being stored**
/// and **where it goes**:
///
/// * The credential is no longer a hand-pasted 64-hex daemon token that owns
///   the entire memory store. It is a refresh token the server minted, scoped
///   to one account, revocable from the server (`POST /auth/logout`, or
///   automatically the moment a replay is detected), and expiring on its own
///   in 30 days.
/// * The destination is no longer a file. It is the Windows Credential
///   Manager / macOS Keychain / libsecret — encrypted at rest, scoped to the
///   OS user, not readable by another user on the same machine.
///
/// The old reasoning rejected *a plaintext file*. It never rejected *a
/// keychain*, because at the time there was nothing worth putting in one.
///
/// ## What this buys
///
/// Closing the app and reopening it no longer asks for a password. That was
/// the entire complaint, and **it is not fixable on the server alone**: the
/// daemon can mint a 30-day credential all it likes, but a client that forgets
/// it on exit makes the user type their password every single launch.
///
/// ## Web is still different
///
/// See `token_store_web.dart`. A browser has no keychain, and everything a tab
/// can persist is readable by any script on the same origin. The choice there
/// is only about how long the exposure lasts.

/// Whether this platform can remember a credential across launches.
const bool kCanRememberToken = true;

/// Shown under the login form.
const String kTokenStorageNote =
    '登录状态保存在系统凭据管理器里（Windows 凭据管理器 / macOS 钥匙串），'
    '不是明文文件。下次打开不用再登录；点「退出登录」会立刻清掉，'
    '并让服务端作废这台设备上的会话。';

// Windows / Linux / macOS 的默认实现已经是 DPAPI / libsecret / Keychain，
// 不需要额外配置。刻意**不**写 Android 的选项：那个参数在 v10 里已经废弃
// （Google 弃了 Jetpack Security），写上去只会留下一条编译警告和一个
// 将来会失效的承诺 —— 等真加移动端时再照当时的实现重新决定
const _storage = FlutterSecureStorage();

/// 存 refresh token 用的键。
///
/// 带上应用前缀：系统凭据库是**全机共享**的命名空间，一个叫
/// `refresh_token` 的键迟早会跟别的软件撞上。
const String _refreshKey = 'cortex.refresh_token';

/// 不需要用户输入任何东西就能拿到的凭据。
///
/// **顺序有意义**：记住的凭据优先于环境变量。登录过的人期待自己还是那个
/// 账号，而某人 shell profile 里一条陈旧的 `CORTEXD_TOKEN` 不该把他悄悄
/// 换回单用户那条老路。
Future<String?> readRememberedToken() async {
  try {
    final stored = await _storage.read(key: _refreshKey);
    if (stored != null && stored.isNotEmpty) return stored;
  } on Object catch (_) {
    // 钥匙串锁着、libsecret 没跑、条目损坏 —— 都落在这里。没有一种该让
    // 应用起不来，它们的意思都只是「你得重新登录一次」，而那正是接下来会发生的
  }
  return readSeedToken();
}

/// 账号体系之前那条逃生口：环境变量里的预共享 token。
///
/// **仍然认**，而且是有意的 —— CLI、现有安装、生产部署都还在用它，
/// 一次性切断会让它们当天全部失联。
String? readSeedToken() {
  final raw = Platform.environment[kTokenEnvVar]?.trim();
  return (raw == null || raw.isEmpty) ? null : raw;
}

Future<void> rememberToken(String token) async {
  try {
    await _storage.write(key: _refreshKey, value: token);
  } on Object catch (e) {
    // 存不进去不等于登录失败 —— 内存里这次会话好好的。**要说出来**，
    // 否则用户会在下次打开时才发现又要输密码，而那时已经没人知道为什么
    // ignore: avoid_print
    print('登录状态没能存进系统凭据库（下次打开需要重新登录）：$e');
  }
}

Future<void> forgetToken() async {
  try {
    await _storage.delete(key: _refreshKey);
  } on Object catch (_) {
    // 删一个本来就不在的键，或者够不着钥匙串。两种都没有后续动作 ——
    // 服务端那侧的作废在调到这里之前就已经发生了
  }
}

/// 把「换 refresh token」这件事串行化的锁。桌面端**同进程只有一个**
/// `AuthController`，`_refreshInFlight ??=` 已经把并发续期收敛成一次 ——
/// 这里直接执行就够了。真正需要锁的是 Web（多标签页共享 localStorage，
/// 各自的内存里却各有一份轮换状态），见 `token_store_web.dart` 的同名实现。
Future<T> withRefreshLock<T>(Future<T> Function() body) => body();
