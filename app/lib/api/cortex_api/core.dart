part of '../cortex_api.dart';

mixin CortexApiCore {
  /// Human-readable name of the active backend, shown in the status strip.
  String get label;

  /// `GET /health` — the **only** unauthenticated route.
  ///
  /// That is what makes it usable as the login gate's probe: it answers
  /// `auth: "token" | "disabled"` to a client that has no credential yet, which
  /// is the one question such a client needs answered before it can decide
  /// whether to demand one from the user.
  Future<HealthStatus> health();

  /// `GET /sandbox/health` —— 「这个部署跑不跑得了**云端对话**」。
  ///
  /// 与 [health] 分成两次请求，因为生产上它们**根本不是同一个进程**：
  /// 边缘按路径分流，`/health` 归记忆服务、这一条才是 agent 编排服务。
  /// 为什么 `/health` 通了还不够，见 [SandboxHealth] 的文档。
  ///
  /// 也是免认证的公开路由（消费者是配不了凭据的探针），所以登录之前
  /// 就能问 —— 与 [health] 同一个理由。
  ///
  /// # **不抛异常**，每一种失败都是一个要显示的答案
  ///
  /// 404、一张 SPA 回落的网页、网关的 502 —— 这些在别处是故障，在这里
  /// 分别是「这个部署没有沙箱」与「有但现在跑不起来」，都是要画到界面上
  /// 的能力说明。让它抛的话，每个调用方都得自己判一次「这个错是不是其实
  /// 正常」，而判漏的那一处会把「自托管没有沙箱」画成一个红框，
  /// 让用户去修一个没坏的东西。与 `localWorkspaceRootProvider` 同一立场。
  Future<SandboxHealth> sandboxHealth();

  void dispose();
}
