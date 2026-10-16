part of '../mock_cortex_api.dart';

mixin _ProjectsMock on MockCortexApiBase {
  @override
  Future<List<Project>> projects() async {
    await _latency(120);
    // 计数当场算，不存 —— 存的那份迟早会和 `_sessions` 对不上，
    // 而对不上的表现是删除确认里写着「3 个会话」但只有 1 个
    return List.unmodifiable(_projects.map(_withCount));
  }

  Project _withCount(Project p) => p.copyWith(
    sessionCount: _sessions.where((s) => s.projectId == p.id).length,
  );

  @override
  Future<Project> createProject(String name) async {
    await _latency(110);
    final trimmed = name.trim();
    if (trimmed.isEmpty) {
      throw const CortexApiException('项目名不能为空白', statusCode: 400);
    }
    final project = Project(
      id: 'prj_${DateTime.now().microsecondsSinceEpoch}',
      name: trimmed,
      createdAt: DateTime.now(),
    );
    _projects.add(project);
    return project;
  }

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) async {
    await _latency(110);
    final index = _projects.indexWhere((p) => p.id == id);
    if (index == -1) {
      throw CortexApiException('project $id 不存在', statusCode: 404);
    }
    if (name == null && pinned == null) {
      throw const CortexApiException(
        '请求体里没有任何要改的字段（name / pinned）',
        statusCode: 400,
      );
    }
    final trimmed = name?.trim();
    if (trimmed != null && trimmed.isEmpty) {
      throw const CortexApiException('项目名不能为空白', statusCode: 400);
    }
    // 改名与置顶各自生效 —— 与服务端那侧「两条事件」一一对应
    final updated = _projects[index].copyWith(name: trimmed, pinned: pinned);
    _projects[index] = updated;
    return _withCount(updated);
  }

  /// 删的只有这一层分组。
  ///
  /// 会话**留在 [_sessions] 里**，只是 `projectId` 清空 —— 这正是服务端
  /// 承诺的语义，而夹具如果顺手把会话也删了，界面上那句「会话不会丢」
  /// 就会在唯一能离线演示的模式里当场被证伪。
  @override
  Future<void> deleteProject(String id) async {
    await _latency(130);
    final index = _projects.indexWhere((p) => p.id == id);
    if (index == -1) {
      throw CortexApiException('project $id 不存在', statusCode: 404);
    }
    _projects.removeAt(index);
    for (var i = 0; i < _sessions.length; i++) {
      if (_sessions[i].projectId == id) {
        _sessions[i] = _sessions[i].copyWith(projectId: null);
      }
    }
  }
}
