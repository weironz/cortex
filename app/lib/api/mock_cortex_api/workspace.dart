part of '../mock_cortex_api.dart';

mixin _WorkspaceMock on MockCortexApiBase {
  /// mock 形态下**这台机器在线且可接入**。
  ///
  /// 回空列表会让「我的机器」那一页在离线演示里是一片空白 —— 而空白与
  /// 「这个部署没有任何机器」长得一模一样，看到的人分不出是没数据还是
  /// 夹具里这台机器的开关状态。**开着** —— 那一档才是要看的：
  /// 关着长什么样一目了然，而「开着时界面有没有把那句话说破」
  /// 才是需要盯着看的（安全不变量 4）。
  bool _attach = true;

  @override
  Future<LocalAttach> localAttach() async =>
      LocalAttach(enabled: _attach, machineHint: 'WILLOPTPC');

  @override
  Future<LocalAttach> setLocalAttach(bool enabled) async {
    _attach = enabled;
    return localAttach();
  }

  /// 没实现（`_search` 上面那段同一个理由）。
  ///
  /// 两行：一台开了远程接入的（常态），一台没开的（那一档的样子也要看得见）。
  @override
  Future<List<AgentPresence>> agents({String? session}) async {
    return [
      AgentPresence(
        agentId: 'mock-a1',
        machineHint: 'WILLOPTPC',
        lastSeenSecs: 3,
        sessionCount: 12,
        hasSession: session == null ? null : true,
        attachable: true,
      ),
      AgentPresence(
        agentId: 'mock-a2',
        machineHint: 'macbook-air',
        lastSeenSecs: 41,
        sessionCount: 2,
        hasSession: session == null ? null : false,
      ),
    ];
  }

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async {
    throw const CortexApiException(
      '模拟后端没有本地工作区端点，改走 PATCH /sessions/{id}。',
      statusCode: 404,
    );
  }

  /// 三条本地工作空间路由一律「这个后端没有」。
  ///
  /// 模拟后端没有文件系统，凭空回一个路径会让界面显示一个磁盘上不存在的
  /// 目录 —— 而调用方对 404 已经有正确的处理（当成「这台机器上没有本机」）。
  @override
  Future<LocalWorkspaceRoot> localWorkspaceRoot() async =>
      throw const CortexApiException('模拟后端没有本地工作空间。', statusCode: 404);

  @override
  Future<LocalWorkspaceRoot> setLocalWorkspaceRoot(String path) async =>
      throw const CortexApiException('模拟后端没有本地工作空间。', statusCode: 404);

  @override
  Future<String> createLocalWorkspace({
    required String name,
    String? projectId,
  }) async => throw const CortexApiException('模拟后端没有本地工作空间。', statusCode: 404);

  @override
  Future<String?> autoBindLocalWorkspace(String id) async =>
      throw const CortexApiException('模拟后端没有本地工作空间。', statusCode: 404);

  // ── MCP ────────────────────────────────────────────
  //
  // 这一族**不**回 404，与上面的工作空间不同。理由是它们要撑起界面的两个
  // 状态：一台连上了、一台没连上且带着原因。回 404 的话那两个状态在 mock
  // 下永远看不见，而「连不上时长什么样」恰恰是最需要看一眼的那个。
}
