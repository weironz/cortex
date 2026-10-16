part of '../cortex_api.dart';

mixin CortexApiAuth {
  /// `POST /auth/ticket` — trade the long-lived token for a 60-second one.
  ///
  /// Exists because `WebSocket` (and `EventSource`, and `<img src>`) cannot
  /// carry an `Authorization` header in a browser — a hard API limitation, not
  /// something to engineer around. The ticket, and only the ticket, is allowed
  /// into a URL: query strings reach access logs, reverse-proxy logs and
  /// browser history, and a credential that expires in a minute survives that
  /// far better than one that never expires.
  ///
  /// Authenticated itself, so it doubles as the cheapest possible "is this
  /// token accepted?" probe — see `AuthController.signIn`.
  Future<AuthTicket> issueTicket();

  /// `POST /auth/login` —— 用户名 + 密码换一对令牌。
  ///
  /// 拿回来的 `refreshToken` 是**「下次打开不用再登录」的全部依据**，
  /// 调用方必须把它存进平台的凭据库（见 `app/lib/auth/`）。
  Future<AuthTokens> login(String username, String password);

  /// `POST /auth/register` —— 开号并直接换回一对令牌（注册即登录）。
  ///
  /// 只在部署开着注册（`/health` 的 `open_registration`）时才该被调用 ——
  /// 关着的部署回 403，正文里写着管理员该怎么开。界面靠那个字段决定
  /// **摆不摆**入口；这条本身不做那个判断。
  Future<AuthTokens> register(String username, String password);

  /// `GET /auth/profile` —— 昵称、头像有没有、以及删号的倒计时。
  ///
  /// 与 `whoami` 分开：那一条每次启动都问（我是谁、库在哪），这一条只有
  /// 账号页要。合成一条会让每次冷启动多读一次头像的元信息。
  Future<Profile> profile();

  /// `PATCH /auth/profile` —— 目前只有昵称。
  ///
  /// [nickname] 用 `Option` 的两层语义：**不传** = 这次不动它，
  /// 传 `null`（即 `const Nullable(null)`）= 清空。Dart 没有
  /// `Option<Option<T>>`，所以这里用一个显式的包装类 [Patch]。
  Future<Profile> updateProfile({Patch<String?>? nickname});

  /// `PUT /auth/avatar` —— body 就是图片字节。
  ///
  /// 服务端按**魔数**认 PNG / JPEG / WebP，不看这里说什么，也不收 SVG
  /// （它能带脚本，而头像会被当图片直接渲染）。上限 256 KiB。
  Future<Profile> putAvatar(Uint8List bytes);

  /// `DELETE /auth/avatar`
  Future<Profile> deleteAvatar();

  /// `GET /auth/avatar/{userId}` —— 取头像的字节。
  ///
  /// **回字节而不是 URL**，与 [blobBytes] 同一条理由：界面从自己取到的
  /// 字节渲染，`Image.network` 那条路 mock 数据源答不出来，而且带不了
  /// `Authorization`。
  ///
  /// 没有头像时抛 404（`isNotFound`）—— 调用方据此画默认头像。
  Future<Uint8List> avatarBytes(String userId);

  /// `POST /auth/email/start` —— 往这个地址发一封验证码。
  ///
  /// **绑定与换绑是同一条路**：换绑就是「再验一次新地址」。
  /// 这个部署没配邮件通道时回 501 —— 界面靠 [Profile.mailAvailable]
  /// 决定摆不摆入口，不该走到这里才发现。
  Future<void> startEmailBinding(String email);

  /// `POST /auth/email/verify` —— 把验证码换成一次真正的绑定。
  Future<Profile> verifyEmail(String code);

  /// `DELETE /auth/email` —— 解绑，要密码。
  ///
  /// 邮箱将来是找回账号的凭据之一，摘掉它是**削弱账号安全**的动作 ——
  /// 与删号同一族，所以不能只凭 token。
  Future<Profile> unbindEmail(String password);

  /// `POST /auth/password` —— 改口令。
  ///
  /// ⚠️ 服务端这条路 2026-08 就有了，**而客户端一直没接** —— 于是设置里
  /// 根本没有改密码的入口。典型的「造好没人接」。
  ///
  /// 成功之后**所有设备都会被登出**（服务端作废整个 refresh 家族），
  /// 包括这一台。调用方要据此把人送回登录页，而不是假装什么都没发生。
  Future<void> changePassword(String oldPassword, String newPassword);

  /// `DELETE /auth/account` —— **排期**删除，不是当场删。
  ///
  /// 要密码：这个动作会销毁整片 schema（全部会话、消息、附件）。拿到
  /// access token 的路子比拿到密码多得多，光凭 token 就能删号等于把
  /// 不可逆操作放在最低的那道门后面。
  ///
  /// 回的 [Profile] 里带着 `purgeAfter` —— 那是冷静期的终点，界面要显示它。
  Future<Profile> deleteAccount(String password);

  /// `POST /auth/account/restore` —— 冷静期内反悔。
  ///
  /// ⚠️ 排期期间**登录是被拒的**，所以只有排期那一刻还没过期的那把 access
  /// token 能撤销（15 分钟）。超过就要管理员帮忙 —— 界面必须把这一条
  /// 说清楚，否则用户会以为「随时能反悔」。
  Future<Profile> restoreAccount();

  /// `POST /auth/refresh` —— 用长效凭据换一对新的。
  ///
  /// 每次都会**轮转**：服务端把旧的作废并签一个新的。所以调用方必须
  /// 存下新的那个 —— 继续用旧的会被判成重放，而重放会让整条链一起失效。
  Future<AuthTokens> refreshSession(String refreshToken);

  /// `GET /auth/me` —— 当前这把凭据对应的是谁。
  ///
  /// 返回 null 有两种意思，**对调用方是同一种**（没有名字可显示）：
  /// 这个后端没有这个端点（老服务端），或者压根没有账号体系
  /// （mock / 关掉认证的部署）。两种都不该让账号栏消失 ——
  /// 它还挂着设置与退出登录。
  Future<Account?> whoAmI();

  /// `POST /auth/logout` —— 让服务端作废这条链。
  ///
  /// 与「本地删掉副本」不是一回事：本地删除只让这台机器忘了，
  /// 而已经泄露出去的那一份照样能用到 30 天后。
  Future<void> logout(String refreshToken);
}
