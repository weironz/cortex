/// MCP 配置的客户端模型。与 `cortex-local` 的 `local_mcp.rs` 一一对应。
///
/// # 这里**没有**环境变量的值
///
/// 服务端只回名字（见那边的模块文档）：MCP server 的 env 常常就是 API key，
/// 一旦回到界面就会出现在截图、录屏、和每一次「帮我看看这个配置」里。
///
/// 于是改配置时客户端手上没有旧值，写回去做的是**合并** —— 界面上那些
/// 已有的变量显示成不可编辑的芯片，要改就重填、要删走 `removeEnv`。
library;

/// 一台 server 现在什么样。
class McpServer {
  const McpServer({
    required this.name,
    required this.transport,
    required this.commandLine,
    required this.envNames,
    required this.trust,
    required this.disabled,
    required this.connected,
    required this.tools,
    this.error,
  });

  final String name;

  /// `stdio` 或 `http`。
  final String transport;

  /// **将要执行的那条命令**（或 HTTP 的 URL）。
  ///
  /// 服务端算好的，客户端只显示。加一台 server = 在这台机器上跑任意进程，
  /// 而这串东西是用户能看到的唯一凭据 —— 客户端自己拼的话，界面显示的和
  /// 真正执行的会是两回事。
  final String commandLine;

  /// 环境变量 / 请求头的**名字**。见库文档。
  final List<String> envNames;

  /// `ask` / `write` / `trusted`。
  final String trust;

  final bool disabled;
  final bool connected;
  final List<McpTool> tools;

  /// 连不上的原因。连上时是 `null`。
  final String? error;

  factory McpServer.fromJson(Map<String, dynamic> json) => McpServer(
    name: json['name'] as String? ?? '',
    transport: json['transport'] as String? ?? 'stdio',
    commandLine: json['command_line'] as String? ?? '',
    envNames:
        (json['env_names'] as List?)?.map((e) => '$e').toList() ?? const [],
    trust: json['trust'] as String? ?? 'ask',
    disabled: json['disabled'] as bool? ?? false,
    connected: json['connected'] as bool? ?? false,
    tools:
        (json['tools'] as List?)
            ?.whereType<Map<String, dynamic>>()
            .map(McpTool.fromJson)
            .toList() ??
        const [],
    error: json['error'] as String?,
  );
}

/// 一个外来工具。`name` 是**加过前缀的全名**（`mcp__server__tool`）——
/// 模型看到的就是这个，剥掉前缀会让界面和日志对不上。
class McpTool {
  const McpTool({required this.name, required this.description});

  final String name;
  final String description;

  factory McpTool.fromJson(Map<String, dynamic> json) => McpTool(
    name: json['name'] as String? ?? '',
    description: json['description'] as String? ?? '',
  );
}

/// `GET /local/mcp` 的整份响应。
class McpConfigView {
  const McpConfigView({required this.path, required this.servers});

  /// 配置文件在哪。要显示出来 —— 手编那条路得有个落点。
  final String path;
  final List<McpServer> servers;

  static const empty = McpConfigView(path: '', servers: []);

  bool get isEmpty => servers.isEmpty;

  int get connectedCount => servers.where((s) => s.connected).length;

  /// 配了但没连上的。`disabled` 的不算 —— 那是用户自己关的，不是故障。
  int get brokenCount =>
      servers.where((s) => !s.connected && !s.disabled).length;

  int get toolCount =>
      servers.fold(0, (sum, s) => sum + s.tools.length);

  factory McpConfigView.fromJson(Map<String, dynamic> json) => McpConfigView(
    path: json['path'] as String? ?? '',
    servers:
        (json['servers'] as List?)
            ?.whereType<Map<String, dynamic>>()
            .map(McpServer.fromJson)
            .toList() ??
        const [],
  );
}

/// 粘贴解析出来的一台，**还没落盘**。
class McpParsedServer {
  const McpParsedServer({
    required this.name,
    required this.transport,
    required this.commandLine,
    required this.config,
    required this.conflicts,
  });

  final String name;
  final String transport;

  /// 确认屏上要原样显示的那一串。
  final String commandLine;

  /// 原样的配置，确认后**原封不动**发回去。
  ///
  /// 客户端不重新拼一份：那等于把服务端刚做过的解析再做一遍，
  /// 而两份解析不一致的症状是「预览的和装上的不是同一个东西」。
  final Map<String, dynamic> config;

  /// 这个名字已经被占了 —— 界面据此提示「会覆盖」。
  final bool conflicts;

  factory McpParsedServer.fromJson(Map<String, dynamic> json) =>
      McpParsedServer(
        name: json['name'] as String? ?? '',
        transport: json['transport'] as String? ?? 'stdio',
        commandLine: json['command_line'] as String? ?? '',
        config: Map<String, dynamic>.from(json['config'] as Map? ?? const {}),
        conflicts: json['conflicts'] as bool? ?? false,
      );
}

/// 官方注册表里的一条。
class McpRegistryEntry {
  const McpRegistryEntry({
    required this.name,
    required this.description,
    required this.version,
    required this.suggestedName,
    required this.installs,
    this.title,
    this.repository,
  });

  /// 注册表里的全名，形如 `io.github.foo/bar`。
  final String name;
  final String? title;
  final String description;
  final String version;
  final String? repository;

  /// 建议的 server 名。用户可以改。
  final String suggestedName;

  /// 各种装法。**空的意味着装不了**（我们不认那种包类型）——
  /// 界面要把它标成灰的，而不是给一个点了没反应的按钮。
  final List<McpInstall> installs;

  bool get installable => installs.isNotEmpty;

  String get displayTitle => (title?.isNotEmpty ?? false) ? title! : name;

  factory McpRegistryEntry.fromJson(Map<String, dynamic> json) =>
      McpRegistryEntry(
        name: json['name'] as String? ?? '',
        title: json['title'] as String?,
        description: json['description'] as String? ?? '',
        version: json['version'] as String? ?? '',
        repository: json['repository'] as String?,
        suggestedName: json['suggested_name'] as String? ?? 'server',
        installs:
            (json['installs'] as List?)
                ?.whereType<Map<String, dynamic>>()
                .map(McpInstall.fromJson)
                .toList() ??
            const [],
      );
}

/// 一种装法。
class McpInstall {
  const McpInstall({
    required this.kind,
    required this.server,
    required this.env,
  });

  /// `npm` / `pypi` / `oci` / `nuget` / `remote`。
  final String kind;

  /// 可直接 PUT 回去的那份配置。
  final Map<String, dynamic> server;

  /// 这台 server 认的环境变量。**值不在里面** —— 要用户填。
  final List<McpEnvVar> env;

  factory McpInstall.fromJson(Map<String, dynamic> json) => McpInstall(
    kind: json['kind'] as String? ?? '',
    server: Map<String, dynamic>.from(json['server'] as Map? ?? const {}),
    env:
        (json['env'] as List?)
            ?.whereType<Map<String, dynamic>>()
            .map(McpEnvVar.fromJson)
            .toList() ??
        const [],
  );
}

/// 一个要用户填的环境变量。
class McpEnvVar {
  const McpEnvVar({
    required this.name,
    required this.required_,
    required this.secret,
    this.description,
    this.defaultValue,
  });

  final String name;
  final String? description;

  /// 不填就跑不起来。界面要拦住「必填还空着就点安装」。
  ///
  /// 字段名带下划线是因为 `required` 是 Dart 关键字。
  final bool required_;

  /// 是密钥。输入框要遮住，且**不留在任何日志里**。
  final bool secret;

  final String? defaultValue;

  factory McpEnvVar.fromJson(Map<String, dynamic> json) => McpEnvVar(
    name: json['name'] as String? ?? '',
    description: json['description'] as String?,
    required_: json['required'] as bool? ?? false,
    secret: json['secret'] as bool? ?? false,
    defaultValue: json['default'] as String?,
  );
}
