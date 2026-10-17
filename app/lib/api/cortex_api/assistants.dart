part of '../cortex_api.dart';

mixin CortexApiAssistants {
  /// `GET /assistants` —— 全部智能体。
  ///
  /// ⚠️ **路由叫 assistants 不叫 agents**：服务端那条 `/agents` 是
  /// 「哪些本机 agent 进程在线」，两个 agent 不是一回事。
  ///
  /// 老服务端答 404，调用方按 [CortexApiException.isUnsupported] 优雅降级。
  Future<List<Assistant>> assistants();

  /// `POST /assistants`
  Future<Assistant> createAssistant(Assistant draft);

  /// `PATCH /assistants/{id}` —— 只发真的要改的那几个字段。
  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,

    /// 整份替换（不是增量）。`null` = 这次不改它。
    List<String>? disabledTools,
  });

  /// `DELETE /assistants/{id}`
  ///
  /// **不影响已经用它聊过的会话**：人设是逐轮带的，历史一个字都不会变。
  Future<void> deleteAssistant(String id);
}

mixin CortexApiSkills {
  /// `GET /skills` —— 全部技能（**含正文**，设置页要编辑它）。
  ///
  /// ⚠️ 发给 `/chat` 的那一份**不含正文**（`Skill.toBrief()`）——
  /// 分层的意义就在那一点。
  ///
  /// 老服务端答 404，调用方按 [CortexApiException.isUnsupported] 优雅降级。
  Future<List<Skill>> skills();

  /// `POST /skills`
  ///
  /// 重名会被拒（名字在服务端带 UNIQUE：模型用它取正文，重名会取错）。
  Future<Skill> createSkill(Skill draft);

  /// `PATCH /skills/{id}` —— 只发真的要改的那几个字段。
  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  });

  /// `DELETE /skills/{id}`
  ///
  /// **不影响已经聊过的会话**：目录是逐轮带的、正文是当场取回来的，
  /// 两样都已经落进历史里的消息了。
  Future<void> deleteSkill(String id);
}
