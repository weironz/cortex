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
}
