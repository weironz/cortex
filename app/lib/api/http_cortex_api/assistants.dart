part of '../http_cortex_api.dart';

mixin _AssistantsEndpoints on HttpCortexApiBase {
  @override
  Future<List<Assistant>> assistants() async {
    final body = await _getJson('/assistants');
    return asObjectList(
      body['assistants'],
    ).map(Assistant.fromJson).toList(growable: false);
  }

  @override
  Future<Assistant> createAssistant(Assistant draft) async =>
      Assistant.fromJson(
        await _postJson('/assistants', {
          'name': draft.name,
          'description': draft.description,
          'instructions': draft.instructions,
          'icon': draft.icon,
          'model': ?draft.model,
          'source': ?draft.source,
        }),
      );

  @override
  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,
    List<String>? disabledTools,
  }) async => Assistant.fromJson(
    await _patchJson('/assistants/${Uri.encodeComponent(id)}', {
      'name': ?name,
      'description': ?description,
      'instructions': ?instructions,
      'icon': ?icon,
      'disabled_tools': ?disabledTools,
    }),
  );

  @override
  Future<void> deleteAssistant(String id) =>
      _noContent('DELETE', '/assistants/${Uri.encodeComponent(id)}');
}

mixin _SkillsEndpoints on HttpCortexApiBase {
  @override
  Future<List<Skill>> skills() async {
    final body = await _getJson('/skills');
    return asObjectList(
      body['skills'],
    ).map(Skill.fromJson).toList(growable: false);
  }

  @override
  Future<Skill> createSkill(Skill draft) async => Skill.fromJson(
    await _postJson('/skills', {
      'name': draft.name,
      'description': draft.description,
      'instructions': draft.instructions,
    }),
  );

  @override
  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) async => Skill.fromJson(
    await _patchJson('/skills/${Uri.encodeComponent(id)}', {
      'name': ?name,
      'description': ?description,
      'instructions': ?instructions,
      'enabled': ?enabled,
    }),
  );

  @override
  Future<void> deleteSkill(String id) =>
      _noContent('DELETE', '/skills/${Uri.encodeComponent(id)}');
}
