part of '../mock_cortex_api.dart';

mixin _McpMock on MockCortexApiBase {
  McpConfigView _mcp = const McpConfigView(
    path: '<mock>/mcp.json',
    servers: [
      McpServer(
        name: 'filesystem',
        transport: 'stdio',
        commandLine: 'npx -y @modelcontextprotocol/server-filesystem /tmp',
        envNames: [],
        trust: 'ask',
        disabled: false,
        connected: true,
        tools: [
          McpTool(name: 'mcp__filesystem__read_file', description: '读一个文件'),
          McpTool(name: 'mcp__filesystem__list_directory', description: '列目录'),
        ],
      ),
      McpServer(
        name: 'docs',
        transport: 'http',
        commandLine: 'https://docs.example.com/mcp',
        envNames: ['AUTHORIZATION'],
        trust: 'trusted',
        disabled: false,
        connected: false,
        tools: [],
        error: '连不上 https://docs.example.com/mcp：dns error',
      ),
    ],
  );

  @override
  Future<McpConfigView> mcpConfig() async => _mcp;

  @override
  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  }) async {
    final rest = _mcp.servers.where((s) => s.name != name).toList();
    _mcp = McpConfigView(
      path: _mcp.path,
      servers: [
        ...rest,
        McpServer(
          name: name,
          transport: config.containsKey('url') ? 'http' : 'stdio',
          commandLine: switch (config) {
            {'url': final String u} => u,
            {'command': final String c, 'args': final List<dynamic> a} =>
              '$c ${a.join(' ')}',
            {'command': final String c} => c,
            _ => name,
          },
          envNames:
              (config['env'] as Map?)?.keys.map((k) => '$k').toList() ??
              const [],
          trust: trust,
          disabled: disabled,
          connected: true,
          tools: const [],
        ),
      ]..sort((a, b) => a.name.compareTo(b.name)),
    );
    return _mcp;
  }

  @override
  Future<McpConfigView> deleteMcpServer(String name) async {
    _mcp = McpConfigView(
      path: _mcp.path,
      servers: _mcp.servers.where((s) => s.name != name).toList(),
    );
    return _mcp;
  }

  @override
  Future<McpConfigView> reloadMcp() async => _mcp;

  @override
  Future<List<McpParsedServer>> parseMcpPaste(String text) async {
    if (text.trim().isEmpty) {
      throw const CortexApiException('粘进来的是空的', statusCode: 400);
    }
    // 名字取最后一段，够这个替身用了 —— 真正的判形在 Rust 那侧，
    // 这里只是让界面能走完「粘贴 → 预览 → 确认」那条路
    final name = text
        .trim()
        .split(RegExp(r'[\s/@]+'))
        .last
        .replaceAll(RegExp(r'[^a-zA-Z0-9._-]'), '');
    return [
      McpParsedServer(
        name: name.isEmpty ? 'server' : name,
        transport: text.startsWith('http') ? 'http' : 'stdio',
        commandLine: text.trim(),
        config: text.startsWith('http')
            ? {'url': text.trim()}
            : {'command': text.trim()},
        conflicts: _mcp.servers.any((s) => s.name == name),
      ),
    ];
  }

  @override
  Future<List<McpRegistryEntry>> searchMcpRegistry(String query) async {
    await Future<void>.delayed(const Duration(milliseconds: 120));
    return const [
      McpRegistryEntry(
        name: 'io.github.example/notes',
        title: 'Notes',
        description: '一个用来演示注册表列表长什么样的条目。',
        version: '1.2.0',
        suggestedName: 'notes',
        installs: [
          McpInstall(
            kind: 'npm',
            server: {
              'command': 'npx',
              'args': ['-y', 'notes-mcp@1.2.0'],
            },
            env: [],
          ),
        ],
      ),
      McpRegistryEntry(
        name: 'io.github.example/unsupported',
        description: '包类型我们不认，所以它装不了 —— 界面该把它标成灰的。',
        version: '0.1.0',
        suggestedName: 'unsupported',
        installs: [],
      ),
    ];
  }
}
