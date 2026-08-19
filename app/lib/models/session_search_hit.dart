import 'json.dart';

/// 一条搜索结果（`GET /sessions/search`）—— **一个会话一条**。
///
/// 服务端刻意不按消息逐条回：一次搜索在同一段对话里常常命中十几条，
/// 逐条铺开会让结果页整屏都是同一个会话，而用户在问的是
/// 「哪几段对话提到过它」。
class SessionSearchHit {
  const SessionSearchHit({
    required this.sessionId,
    this.title,
    this.archived = false,
    this.titleMatch = false,
    this.hitCount = 0,
    this.excerpt,
  });

  factory SessionSearchHit.fromJson(Map<String, dynamic> json) =>
      SessionSearchHit(
        sessionId: asString(json['session_id']),
        title: asStringOrNull(json['title']),
        archived: json['archived'] == true,
        titleMatch: json['title_match'] == true,
        hitCount: asInt(json['hit_count']),
        excerpt: asStringOrNull(json['excerpt']),
      );

  final String sessionId;

  /// 用户设的标题。`null` = 从没改过名 —— 界面回落到自己那套派生规则，
  /// 与侧栏列表保持一致，而不是显示一个裸 id。
  final String? title;
  final bool archived;

  /// 标题本身命中。命中标题时**不显示摘录**：那一行已经把命中的地方
  /// 摆在眼前了，再补一段正文只会挤掉真正有信息的那几条。
  final bool titleMatch;

  /// 这个会话里有几条消息命中。0 = 只有标题命中。
  final int hitCount;

  /// 最近一条命中消息的上下文片段。
  final String? excerpt;
}
