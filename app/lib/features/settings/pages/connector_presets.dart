/// 精选连接器 —— 一键接上几台常用的 MCP server。
///
/// # 这不是第二套机制
///
/// 接上去之后它就是一台普通的 MCP server：同一份 `.mcp.json`、同一个列表、
/// 同一个详情页、同一条删除路径。这里只是**替用户省掉「叫什么包、参数怎么写」
/// 那一步** —— 那一步是绝大多数人卡住的地方，而它跟能力本身毫无关系。
///
/// # ⚠️ 每一条都得是**现在真的装得上**的
///
/// 摆一条装不上的（包名改了、仓库没了）等于让用户点一下、等三十秒、
/// 拿到一句 npm 的报错。这是 CLAUDE.md 约束 2 在界面上的样子：
/// 摆出来的东西就是承诺。下面每一条的包名与版本都在 2026-08-23 当天
/// 对 npm registry 查过：
///
/// | 包 | 当天版本 |
/// |---|---|
/// | `@playwright/mcp` | 0.0.79 |
/// | `@modelcontextprotocol/server-github` | 2025.4.8 |
/// | `@upstash/context7-mcp` | 4.0.3 |
/// | `@modelcontextprotocol/server-pdf` | 1.7.5 |
///
/// **不钉版本**（`-y` 后面不带 `@x.y.z`）：钉了就要有人跟着升，而没人会跟。
/// 不钉的代价是上游发了个坏版本时这里跟着坏 —— 那时详情页会把连接错误
/// 原样显示出来，用户还能删掉重来。
///
/// # 有意不收的两条
///
/// * **`server-filesystem`**（本地文件）—— 我们**已经有**文件工具，而且它们
///   受工作区那道围栏管。再接一台能读整块盘的，等于在围栏边上开一扇没锁的门，
///   而用户完全看不出这两套的区别。
/// * **`server-memory`** —— 这个仓库 2026-08-17 把长期记忆整条拆走了
///   （见 CLAUDE.md 顶上那段）。摆一个叫「记忆」的连接器在这儿，只会让人
///   以为那件事回来了。
library;

/// 一条精选。
class ConnectorPreset {
  const ConnectorPreset({
    required this.id,
    required this.name,
    required this.title,
    required this.description,
    required this.icon,
    required this.config,
    this.envHint,
  });

  /// 稳定标识，只用来判断「这条是不是已经接过了」。
  final String id;

  /// 落盘时那台 server 的名字。**它会变成工具名的前缀**
  /// （`mcp__github__…`），所以要短、要一眼认得出。
  final String name;

  final String title;

  /// 一句话：**它能让模型多做什么**，不是「它是什么」。
  ///
  /// 「基于 Playwright 的浏览器自动化 MCP server」这种写法对着的是
  /// 已经知道答案的人。用户要的是「让它自己开网页、点按钮、填表单」。
  final String description;

  /// 一个 emoji。图标库里没有对得上的，而一排一样的插头图标等于没有图标。
  final String icon;

  /// 原样 PUT 回 `/local/mcp/servers/{name}` 的那份传输配置。
  final Map<String, dynamic> config;

  /// 要用户自己填的那个环境变量（多半是 key）。`null` = 不用填就能跑。
  ///
  /// ⚠️ 值**只经过用户的键盘**：我们既不代填也不代存 —— 它进的是本机那份
  /// `.mcp.json`，而服务端读它的时候只回名字不回值（见 `models/mcp.dart`）。
  final ConnectorEnvHint? envHint;

  bool get needsKey => envHint != null;
}

/// 「这台要一个 key，去哪儿拿」。
class ConnectorEnvHint {
  const ConnectorEnvHint({
    required this.variable,
    required this.label,
    required this.where,
  });

  /// 环境变量名，如 `GITHUB_PERSONAL_ACCESS_TOKEN`。
  final String variable;

  /// 输入框上的标签。
  final String label;

  /// **去哪儿拿这把 key。** 不说的话，用户在这里就卡住了 ——
  /// 一个只写着变量名的输入框回答不了「我上哪儿弄一个」。
  final String where;
}

/// ⚠️ 每条 `args` 的第一个都是 `-y`。
///
/// 它免掉 npx 那句「要装 xxx 吗 (y/N)」—— 那句问话在 stdio 上没人回答得了，
/// 于是这台 server 永远连不上，而日志里什么都看不出来（只是一直没连上）。
/// 加新的精选时别漏。
const connectorPresets = <ConnectorPreset>[
  ConnectorPreset(
    id: 'playwright',
    name: 'browser',
    title: '浏览器',
    description: '让它自己开网页、点按钮、填表单、截图 —— 会读页面结构，不是靠猜坐标。',
    icon: '🌐',
    config: {
      'command': 'npx',
      'args': ['-y', '@playwright/mcp'],
    },
  ),
  ConnectorPreset(
    id: 'context7',
    name: 'docs',
    title: '库文档',
    description: '写代码时现查某个库眼下的用法，而不是凭训练时记下的旧 API。',
    icon: '📚',
    config: {
      'command': 'npx',
      'args': ['-y', '@upstash/context7-mcp'],
    },
  ),
  ConnectorPreset(
    id: 'github',
    name: 'github',
    title: 'GitHub',
    description: '读仓库、翻 issue 与 PR、提交改动 —— 不用再手工把代码贴进对话。',
    icon: '🐙',
    config: {
      'command': 'npx',
      'args': ['-y', '@modelcontextprotocol/server-github'],
    },
    envHint: ConnectorEnvHint(
      variable: 'GITHUB_PERSONAL_ACCESS_TOKEN',
      label: 'GitHub 访问令牌',
      // 光写变量名的话，用户在这里就卡住了
      where:
          'GitHub → Settings → Developer settings → Personal access tokens。'
          '只读仓库的话勾 repo 就够。',
    ),
  ),
  ConnectorPreset(
    id: 'pdf',
    name: 'pdf',
    title: 'PDF',
    description: '把 PDF 里的文字读出来 —— 合同、论文、扫描件都能直接问。',
    icon: '📄',
    config: {
      'command': 'npx',
      'args': ['-y', '@modelcontextprotocol/server-pdf'],
    },
  ),
];
