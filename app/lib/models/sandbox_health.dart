import 'json.dart';

/// 「这个地址跑不跑得了**云端对话**」——`GET /sandbox/health` 的回答。
///
/// # 为什么 `/health` 通了还不够
///
/// `/health` 只证明**这个地址上有个进程在答话**。而一轮云端对话要走完
/// 三段完全不同的链路：agent 编排服务向记忆服务换一把委托凭据、据它拉起
/// 沙箱容器、容器再回连编排服务把结果送回来。这三段里断哪一段，`/health`
/// 都照样 200 —— 于是连接页画绿灯「已连接」，用户回到对话框发一句，
/// 得到的是失败。2026-08-15 真机上就是这个形状。
///
/// 生产上还要更狠一层：边缘按路径分流，`/health` 分给了**记忆服务**，
/// `/sandbox/health` 才是 agent 编排服务。也就是说那条路上的 `/health`
/// 连「编排服务在不在」都没有回答，它答的是另一个进程的近况。
///
/// # 「没有沙箱」不是故障
///
/// 自托管、纯本机、以及所有不接 docker 的部署本来就没有云沙箱，那是正常
/// 形态。所以这里分成三档而不是一个布尔：把 [CloudChatStatus.absent] 与
/// [CloudChatStatus.blocked] 并成一个「不可用」，用户会去修一个没坏的东西。
class SandboxHealth {
  const SandboxHealth({
    required this.status,
    this.version,
    this.reason,
    this.openRegistration,
  });

  final CloudChatStatus status;

  /// 这个部署开着注册吗（agentd 的 health 同时挂在 `/sandbox/health` 上）。
  ///
  /// 登录页在 `/health` 答不出这个字段时（生产边缘把那条分给了记忆服务）
  /// 从这里补问。`null` = 答话的不是 agentd 或版本太老 —— 按关闭处理，
  /// 保守方向：藏一个开着的入口，好过摆一个必然 403 的入口。
  final bool? openRegistration;

  /// agent 编排服务自己的版本号。
  ///
  /// 与 `/health` 报的那个**可能不是同一个进程**的版本（见上文的分流），
  /// 所以两边都要显示 —— 只显示一个，滚更新滚到一半的部署看起来是一致的。
  final String? version;

  /// [CloudChatStatus.blocked] 时**断在哪一段**，写给用户看。
  ///
  /// 不写「云端对话不可用」了事：这三段坏了之后该做的事完全不同
  /// （去把记忆服务起起来 / 去看容器网络 / 等编排服务重启完），
  /// 而一句笼统的失败只能让人来问我们。
  final String? reason;

  /// 这个部署没有云沙箱。**中性事实，不是错误。**
  static const SandboxHealth absent = SandboxHealth(
    status: CloudChatStatus.absent,
  );

  static SandboxHealth blocked(String reason) =>
      SandboxHealth(status: CloudChatStatus.blocked, reason: reason);

  /// agent 编排服务在 `role` 里的自称。
  ///
  /// 判它而不是判「有没有 200」：这条路上答话的**不一定是它**。生产的边缘
  /// 把 `/health` 分给记忆服务；而 Web 那侧 nginx 对认不出的路径是 SPA
  /// 回落 —— `/sandbox/health` 会拿到 **200 + 一张 index.html**，
  /// 看起来完全像成功（CLAUDE.md 里专门记了这一条）。
  static const String _orchestratorRole = 'agent-orchestrator';

  /// 解析编排服务的健康报告。**认不出的一律当「没有沙箱」。**
  factory SandboxHealth.fromJson(Map<String, dynamic> json) {
    if (asString(json['role']) != _orchestratorRole) return absent;
    final version = asStringOrNull(json['version']);
    final openRegistration = json['open_registration'] as bool?;

    // 下面这个字段是**服务端自己去打过**的结果，不是它的自我感觉
    // （见 cortex-agentd 的 health handler）。缺字段一律当成通：
    // 老版本的编排服务不报它们，而据此宣布「跑不了」会把一个能用的部署
    // 说成坏的 —— 这个方向的误报比漏报贵得多。
    if (json['callback_visible_to_sandbox'] == false) {
      return SandboxHealth(
        status: CloudChatStatus.blocked,
        version: version,
        reason: '沙箱容器回连不到 agent 编排服务，每一轮都会在中途断掉。',
        openRegistration: openRegistration,
      );
    }
    return SandboxHealth(
      status: CloudChatStatus.ready,
      version: version,
      openRegistration: openRegistration,
    );
  }
}

/// 云端对话这项能力此刻的三种处境。
enum CloudChatStatus {
  /// 整条链都通。
  ready,

  /// 编排服务在，但它够不着自己必须够着的东西 —— 现在发消息会失败。
  blocked,

  /// 这个地址上压根没有沙箱那一套。**正常形态之一**，不是故障。
  absent,
}
