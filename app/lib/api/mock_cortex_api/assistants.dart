part of '../mock_cortex_api.dart';

mixin _AssistantsMock on MockCortexApiBase {
  /// 智能体。假后端里就这一份，够界面走通「建 / 改 / 删」。
  ///
  /// 预置一个：一个空列表看不出卡片长什么样，而这一页的全部意义就是
  /// 让人一眼看见「我有哪些人设」。
  final List<Assistant> _assistants = [
    const Assistant(
      id: 'MOCKASSIST0001',
      name: '味蕾领航员',
      icon: '🍳',
      description: '一位精通全球美食的资深大厨',
      instructions:
          '你是一位精通全球美食、拥有 20 年烹饪经验的资深大厨，'
          '深谙食材营养学与各种烹饪技巧。',
    ),
  ];

  @override
  Future<List<Assistant>> assistants() async {
    await _latency(90);
    return List.unmodifiable(_assistants);
  }

  @override
  Future<Assistant> createAssistant(Assistant draft) async {
    await _latency(110);
    final name = draft.name.trim();
    if (name.isEmpty) {
      throw const CortexApiException('智能体得有个名字', statusCode: 400);
    }
    final made = Assistant(
      id: 'MOCKASSIST${(_assistants.length + 2).toString().padLeft(4, '0')}',
      name: name,
      description: draft.description,
      instructions: draft.instructions,
      icon: draft.icon,
      model: draft.model,
      source: draft.source,
      createdAt: DateTime.now(),
      updatedAt: DateTime.now(),
    );
    _assistants.insert(0, made);
    return made;
  }

  @override
  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,
    List<String>? disabledTools,
  }) async {
    await _latency(110);
    final i = _assistants.indexWhere((a) => a.id == id);
    if (i == -1) {
      throw CortexApiException('找不到智能体：$id', statusCode: 404);
    }
    final updated = _assistants[i].copyWith(
      name: name?.trim(),
      description: description,
      instructions: instructions,
      icon: icon,
      disabledTools: disabledTools,
    );
    _assistants[i] = updated;
    return updated;
  }

  @override
  Future<void> deleteAssistant(String id) async {
    await _latency(100);
    // 重复删不报错 —— 与服务端那侧同一个约定
    _assistants.removeWhere((a) => a.id == id);
  }
}

mixin _SkillsMock on MockCortexApiBase {
  /// 技能。预置一条：一个空列表看不出这一页长什么样。
  final List<Skill> _skills = [
    const Skill(
      id: 'MOCKSKILL0001',
      name: '周报',
      description: '按公司模板写周报：本周进展 / 下周计划 / 风险',
      instructions:
          '写周报时按三段来：\n'
          '1. 本周进展 —— 每条一句话，带上可验证的结果\n'
          '2. 下周计划 —— 每条要能在一周内做完\n'
          '3. 风险与阻塞 —— 没有就写「无」，不要略过这一段',
    ),
  ];

  @override
  Future<List<Skill>> skills() async {
    await _latency(90);
    return List.unmodifiable(_skills);
  }

  @override
  Future<Skill> createSkill(Skill draft) async {
    await _latency(110);
    final name = draft.name.trim();
    if (name.isEmpty) {
      throw const CortexApiException('非法输入：技能得有个名字', statusCode: 400);
    }
    // 重名在假后端也要拒 —— 界面上那条错误路径才有地方走
    if (_skills.any((s) => s.name == name)) {
      throw CortexApiException('非法输入：已经有一个叫「$name」的技能了', statusCode: 400);
    }
    final made = Skill(
      id: 'MOCKSKILL${(_skills.length + 2).toString().padLeft(4, '0')}',
      name: name,
      description: draft.description.trim(),
      instructions: draft.instructions,
      createdAt: DateTime.now(),
      updatedAt: DateTime.now(),
    );
    _skills.insert(0, made);
    return made;
  }

  @override
  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) async {
    await _latency(90);
    final i = _skills.indexWhere((s) => s.id == id);
    if (i < 0) {
      throw CortexApiException('找不到技能：$id', statusCode: 404);
    }
    final updated = _skills[i].copyWith(
      name: name?.trim(),
      description: description?.trim(),
      instructions: instructions,
      enabled: enabled,
    );
    _skills[i] = updated;
    return updated;
  }

  @override
  Future<void> deleteSkill(String id) async {
    await _latency(70);
    _skills.removeWhere((s) => s.id == id);
  }
}
