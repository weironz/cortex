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
    '浏览器里没有安全的凭据存储：这份凭据会被本页面上运行的任何脚本读到。'
    '勾选则存进 localStorage —— 关掉浏览器再打开还在，直到 30 天后过期'
    '或主动登出。不勾选则只存在于内存中，刷新即失效。';

/// Reads the opt-in copy, if the user made one.
///
/// Storage access can throw outright — a sandboxed iframe or a browser
/// configured to block site data both raise on the *getter*, not on the value.
/// Treated as "nothing stored", because the alternative is a white screen at
/// launch caused by a privacy setting.
///
/// 这份是 **refresh token**，登录/续期时存的。
///
/// # 为什么是 localStorage 而不是 sessionStorage
///
/// 上一版用 sessionStorage，而它**随标签页死**：关掉浏览器再打开，凭据没了，
/// 用户又见一次登录框 —— 可登录页上写的承诺是「记住 30 天，关掉再打开
/// 不用重来」。两种存储对 XSS 的暴露一模一样（同页脚本都读得到），
/// sessionStorage 换来的不是安全，只是把承诺变成谎话。
/// 真正的 30 天上限由服务端的 refresh token 有效期把守，不靠浏览器。
///
/// 读的时候两个都看：sessionStorage 那份是旧版本存下的，认它，用户就不必
/// 因为升级而重新登录一次。
Future<String?> readRememberedToken() async {
  try {
    final raw =
        (web.window.localStorage.getItem(_kKey) ??
                web.window.sessionStorage.getItem(_kKey))
            ?.trim();
    return (raw == null || raw.isEmpty) ? null : raw;
  } on Object {
    // 沙箱 iframe 或禁了站点数据的浏览器在 getter 上就抛。
    // 当成「没存过」—— 另一个选项是启动白屏
    return null;
  }
}

/// Web 上**恒为 null** —— 浏览器没有预共享 token 这回事。
///
/// # 这里曾经是一个把整条登录链打断的 bug
///
/// 上一版写的是 `readRememberedToken() => readSeedToken()`，两个名字读同一个
/// sessionStorage key。但那个 key 里存的是 **refresh token**，而 seed 的
/// 消费者把它当**预共享 token**直接塞进 Authorization 头。后果是一整条链：
///
/// 1. 启动时 `_seeded()` 把 refresh token 放进 access token 的位置，
///    随后每个认证请求 401 —— 登录页上凭空一句「凭据已失效（HTTP 401）」；
/// 2. 更隐蔽的一半：`_bootstrap` 用「remembered ≠ seed」判断要不要续期，
///    两个函数返回同一个值，条件**恒假** —— `restoreSession` 在 Web 上
///    从来没有被调用过，「登录一次管 30 天」在 Web 上根本不存在。
///
/// 桌面端的两个函数是真正不同的东西（环境变量 vs 钥匙串），这个坑只在
/// Web 版的「偷懒转发」里。分开之后：seed 恒 null，续期条件恒真，
/// 启动走的是 refresh 那条正路。
String? readSeedToken() => null;

Future<void> rememberToken(String token) async {
  try {
    web.window.localStorage.setItem(_kKey, token);
    // 旧版本可能在 sessionStorage 留了一份；不清的话登出（只清 local）之后
    // 下次启动又会从那份「复活」。
    web.window.sessionStorage.removeItem(_kKey);
  } on Object {
    // Storage blocked. The token still works for this run; silently keeping it
    // in memory is a better outcome than failing the login the user just
    // completed successfully.
  }
}

Future<void> forgetToken() async {
  try {
    web.window.localStorage.removeItem(_kKey);
    web.window.sessionStorage.removeItem(_kKey);
  } on Object {
    // Nothing was stored if the setter failed earlier.
  }
}
