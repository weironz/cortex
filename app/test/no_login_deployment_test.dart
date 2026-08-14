/// 关掉认证的部署（`CORTEX_AUTH=disabled`）要**完整可用**，不是「能进但少一半」。
///
/// # 这条盯着的那个静默缺陷
///
/// 自托管跑在 127.0.0.1 上、不想要登录的人会设 `CORTEX_AUTH=disabled`。
/// 那种部署根本没有 token。而本地 agent 的启动条件原先只看
/// `token == null` —— 于是这条路上会发生：
///
/// 1. 登录界面被跳过（对的，`/health` 报 `auth: "disabled"`）
/// 2. 本地 agent **静默地不启动**
///
/// 用户看到的是「装了桌面端，但它读不到我本机的文件」，而界面上没有
/// 任何一处说明为什么。两件事本来就没有因果关系：本地 agent 要的是
/// 「能连上 cortexd」，不是「手里有一把 token」。
library;

import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('auth=disabled 的部署不需要 token', () {
    const h = HealthStatus(
      status: 'ok',
      version: '0.1.5',
      database: 'ok',
      auth: 'disabled',
    );
    expect(
      h.requiresToken,
      isFalse,
      reason:
          '这是「跳过登录」的唯一判据。它错了，一个本来不需要登录的'
          '部署会被要求粘一串 token —— 而那串东西根本不存在',
    );
  });

  test('auth=token 的部署仍然要 token', () {
    const h = HealthStatus(
      status: 'ok',
      version: '0.1.5',
      database: 'ok',
      auth: 'token',
    );
    expect(h.requiresToken, isTrue);
  });

  test('认不出的 auth 取值按「需要凭据」处理', () {
    const h = HealthStatus(
      status: 'ok',
      version: '0.1.5',
      database: 'ok',
      auth: 'something-new',
    );
    expect(
      h.requiresToken,
      isTrue,
      reason:
          '不认识的取值必须往严的方向倒。反过来的话，服务端将来加一种'
          '认证形态，老客户端会当成「不用认证」直接放行',
    );
  });

  /// 这条盯的是同一个缺陷的**下一半**。
  ///
  /// 上一半修完之后 agent 会起了，但桌面端连不上它：`CORTEX_AUTH=disabled`
  /// 时用户的 token 是 `null`，启动 agent 那一处写的是
  /// `token ?? _sessionSecret`（对的 —— agent 能执行命令，同机任意进程都
  /// 够得着 127.0.0.1，必须认证），而建 HTTP 客户端那一处只写了 `token`。
  ///
  /// 于是 agent 拿着一把凭据守门，桌面端一个 Authorization 头都不发，
  /// **自己把自己 401 挡在外面**。界面回到登录页说「凭据已失效，请重新
  /// 填写 token」，而那种部署根本没有 token 这回事，怎么填都没用。
  test('远端不认证时，发给本地 agent 的凭据必须与启动它时给的那把是同一个', () {
    const noUser = null;
    final given = localAgentToken(noUser);
    final sent = apiToken(userToken: noUser, onLocalAgent: true);

    expect(
      sent,
      isNotNull,
      reason: '不发凭据的话，守着门的 agent 会把桌面端自己挡在外面',
    );
    expect(
      sent,
      given,
      reason:
          '两处必须是同一个值。它们漂开的症状是「登录页反复弹出、'
          '而这个部署根本没有账号」——从界面上完全看不出根因在本地 agent',
    );
  });

  /// 反过来：这把本机凭据**不能**发给一个真的要认证的 cortexd。
  ///
  /// 发过去换回来的是一个内容完全不同的 401（「这串东西不是有效凭据」，
  /// 而不是「你还没登录」），而两种 401 在界面上长得一模一样。
  test('指向远端时原样用用户的 token，不掺本机那把', () {
    expect(apiToken(userToken: null, onLocalAgent: false), isNull);
    expect(apiToken(userToken: 'real', onLocalAgent: false), 'real');
    expect(
      apiToken(userToken: 'real', onLocalAgent: true),
      'real',
      reason: '有用户 token 时两条路一致 —— 本机那把只是没有 token 时的替补',
    );
  });
}
