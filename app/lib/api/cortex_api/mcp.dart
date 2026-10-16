part of '../cortex_api.dart';

mixin CortexApiMcp {
  /// `GET /local/mcp` — 配置 + 逐台状态 + 每台的工具。
  Future<McpConfigView> mcpConfig();

  /// `PUT /local/mcp/servers/{name}` — 加一台或改一台，落盘后立刻重连。
  ///
  /// [config] 是原样的传输配置（`command`/`args`/`env` 或 `url`/`headers`）。
  /// **env 是合并的**：没给的保持原样，要删的列进 [removeEnv] —— 客户端手上
  /// 从来没有过旧的值（服务端只回名字），所以做不了替换。
  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  });

  /// `DELETE /local/mcp/servers/{name}`
  Future<McpConfigView> deleteMcpServer(String name);

  /// `POST /local/mcp/reload` — 重读文件、重新连。
  ///
  /// 用户手编过配置之后要有一条路能生效，否则「配置文件在这里」那句话
  /// 就是假的：告诉了位置，却要重启才算数。
  Future<McpConfigView> reloadMcp();

  /// `POST /local/mcp/parse` — 粘一段进来，看看会变成什么。**不落盘**。
  ///
  /// 与落盘分成两步是刻意的：加一台 MCP server = 在这台机器上跑任意进程，
  /// 中间必须有一屏让用户看到那条命令行原文。
  Future<List<McpParsedServer>> parseMcpPaste(String text);

  /// `GET /local/mcp/registry?q=` — 代查官方 MCP 注册表。
  Future<List<McpRegistryEntry>> searchMcpRegistry(String query);
}
