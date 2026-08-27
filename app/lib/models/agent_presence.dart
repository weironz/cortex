import 'json.dart';

/// 名下一台**此刻在线**的机器 —— `GET /agents` 里的一行。
///
/// # 为什么它只是「提示」，不是身份
///
/// `machineHint` 通常是主机名，**只拿来给人认机器**，不参与任何判断。
/// 服务端那条论证不变：本机绑定存在那台机器的 `workspaces.json` 里，
/// 「这台机器有没有这个会话的绑定」本身就是设备检查 —— 比一个 id 更硬，
/// id 在重装或克隆之后会骗人。
///
/// # 离线的机器**不在这个列表里**
///
/// 服务端只回还在 TTL 内的（心跳 30 秒一次，TTL 90 秒）。「不在名册里」
/// 与「在名册里但离线」是同一件事，两种表达会让客户端多一条分支，
/// 而那条分支迟早与服务端的判断漂开。
class AgentPresence {
  const AgentPresence({
    required this.agentId,
    required this.machineHint,
    required this.lastSeenSecs,
    required this.sessionCount,
    this.hasSession,
    this.attachable = false,
  });

  factory AgentPresence.fromJson(Map<String, dynamic> json) => AgentPresence(
    agentId: asString(json['agent_id']),
    machineHint: asString(json['machine_hint']),
    lastSeenSecs: asIntOrNull(json['last_seen_secs']) ?? 0,
    sessionCount: asIntOrNull(json['session_count']) ?? 0,
    hasSession: json['has_session'] as bool?,
    attachable: json['attachable'] == true,
  );

  /// 这个 agent **进程**的标识。同一台机器重启之后会换 —— 所以它不能
  /// 当机器身份用，只用来在同一份列表里区分两行。
  final String agentId;

  /// 给人看的机器名（通常是 hostname）。见类文档：**不做任何判断**。
  final String machineHint;

  /// 距上一条心跳多少秒。
  final int lastSeenSecs;

  /// 这台机器报告自己持有多少个会话的绑定。
  ///
  /// **只有个数，没有 id 列表** —— 那是服务端刻意的：整份 id 列表下发
  /// 会让一个诊断接口变成一次会话清单泄露给别的设备。
  final int sessionCount;

  /// 问了 `?session=` 时：这台机器有没有那个会话的绑定。
  final bool? hasSession;

  /// 云端**够得着**它 —— 机器主人开了远程接入，而且此刻真的连得上
  /// （有反向隧道，或直拨地址探得通）。
  ///
  /// 「同意」与「够得着」合成一个布尔是服务端刻意的：分成两个的话，
  /// 客户端要自己判断「同意但连不上」该显示什么，而那个判断迟早与
  /// 服务端的漂开。
  final bool attachable;
}
