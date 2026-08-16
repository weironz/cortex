import 'dart:js_interop';

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
/// **不回落读旧的 sessionStorage 份**。回落读曾经写过，后被审查证伪：
/// sessionStorage 按标签页隔离，旧版本下每个登录过的标签页各持一条独立
/// family —— 用户在 A 标签页登出（清掉 localStorage、作废 A 的 family），
/// B 标签页一刷新，回落读到 B 自己的旧份、续期成功、又写回 localStorage：
/// **明确登出之后凭据在另一个标签页复活**，还从「随标签页死」升格成
/// 「跨重启存活」。迁移的全部收益是老用户少登录一次，换这个不值。
Future<String?> readRememberedToken() async {
  try {
    final raw = web.window.localStorage.getItem(_kKey)?.trim();
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

/// 把「换 refresh token」串行化到**整个 origin**（所有标签页排队）。
///
/// # 为什么必须有它
///
/// refresh token 是一次性轮换的：同一枚被出示两次，服务端按重放处理，
/// **整条 family 作废**（见 `cortex-agentd/src/credentials.rs`，一条测试
/// 钉死了无并发宽限）。凭据搬进 localStorage 之后所有标签页共享一条链，
/// 而每个标签页的内存里各有一份轮换状态 —— 浏览器重启恢复两个标签页，
/// 两个 `_bootstrap` 同时拿同一枚去续，后到的就是重放：两页全被登出，
/// 恰好把「关掉再打开不用重来」变成多标签用户身上的必然失败。
///
/// Web Locks 是跨标签页的进程级锁，正是为这种事设计的。排在后面的调用
/// 要在锁内**重读存储**（见 `AuthController.restoreSession`），用前一个
/// 留下的后继而不是自己手里那枚已作废的。
Future<T> withRefreshLock<T>(Future<T> Function() body) async {
  T? out;
  Object? err;
  StackTrace? st;
  Future<void> run() async {
    try {
      out = await body();
    } catch (e, s) {
      err = e;
      st = s;
    }
  }

  try {
    await web.window.navigator.locks
        .request('cortex.token.refresh', ((JSAny? lock) => run().toJS).toJS)
        .toDart;
  } on Object {
    // 没有 Web Locks 的环境（老浏览器、非安全上下文）：退回无锁执行。
    // 单标签页时无影响；多标签页退回改动前的暴露面，不新增风险。
    await run();
  }
  if (err != null) {
    Error.throwWithStackTrace(err!, st ?? StackTrace.current);
  }
  return out as T;
}
