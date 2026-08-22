import 'package:flutter/material.dart';

import '../../core/theme.dart';
import 'pages/about_page.dart';
import 'pages/appearance_page.dart';
import 'pages/skills_page.dart';
import 'pages/usage_page.dart';
import 'pages/connection_page.dart';
import 'pages/data_page.dart';
import 'pages/mcp_page.dart';
import 'pages/model_page.dart';
import 'pages/model_roles_page.dart';

/// 打开设置。**整屏一页，不是对话框。**
///
/// 名字与文件名保留了旧的（`settings_sheet.dart` / `showSettingsSheet`）：
/// 唯一的调用点在账号栏，改名要动一个与这次无关的文件，而收益是零。
Future<void> showSettingsSheet(BuildContext context) {
  return Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (_) => const SettingsPage(),
      // 设置不是「对话的一部分」，它是另一个地方 —— 全屏路由而不是弹层，
      // 这样返回键、手势返回、以及窗口缩放都按平台的规矩走
      fullscreenDialog: false,
    ),
  );
}

/// 左导航 + 右内容，铺满整个窗口。
///
/// # 为什么从对话框换成整屏
///
/// 上一版是一个按屏幕比例算尺寸的 `Dialog`（宽 78%、高 86%，各自封顶）。
/// 它有两个躲不掉的毛病：
///
/// 1. **最宽的那一页放不下。** 「模型服务」自己就是三栏（导航 + 来源列表 +
///    详情），再套一层限宽的对话框，右边的型号列表被挤成一条缝 ——
///    而那一栏正是信息最密的地方（价格、上下文、能力、开关）。
/// 2. **它不是一个「弹出来问你一句」的东西。** 对话框的形态承诺的是
///    「看一眼就走」，而设置是要待一会儿的地方：填 key、拉列表、逐个开关。
///
/// Cherry Studio 的设置是整屏一页、左上角一个「← 返回」，同一个判断。
///
/// # 分类按「我要改什么」，不按「它属于哪个模块」
///
/// 「模型服务」（有哪些来源、每条开放哪些型号）与「默认模型」（每种活儿
/// 默认用哪个）分成两页，照 Cherry Studio 的分法：前者是**配置**、一次性
/// 的；后者是**偏好**、会反复改。堆在一页里的下场是想换个默认模型的人
/// 得先滚过一屏 key 与端点。
///
/// **导航不分组。** Cherry 那边分了工具 / 偏好 / 系统三组，因为它有二十
/// 来项；我们只有八项，分组会让每组只剩一两条 —— 那时组标题比它组织的
/// 内容还多，纯属噪音。
class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key});

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

/// 一页。
typedef _Section = ({String label, String hint, IconData icon, Widget page});

class _SettingsPageState extends State<SettingsPage> {
  int _index = 0;

  static const _sections = <_Section>[
    (
      label: '连接',
      hint: '数据源 · 后端状态',
      icon: Icons.cable_rounded,
      page: ConnectionPage(),
    ),
    (
      label: '模型服务',
      hint: '供应商 · key · 型号',
      icon: Icons.psychology_outlined,
      page: ModelPage(),
    ),
    (
      label: '默认模型',
      hint: '主 · 快速 · 绘画',
      icon: Icons.tune_rounded,
      page: ModelRolesPage(),
    ),
    // 技能紧挨着「MCP 与工具」：两者都是「模型手上多了什么」。
    // 差别是 MCP 给的是**能力**（去做一件它本来做不到的事），
    // 技能给的是**做法**（同一件事按你的规矩做）
    (
      label: '技能',
      hint: '写好的做法 · 按需取用',
      icon: Icons.auto_stories_outlined,
      page: SkillsPage(),
    ),
    (
      label: 'MCP 与工具',
      hint: '第三方 server',
      icon: Icons.extension_outlined,
      page: McpPage(),
    ),
    (
      label: '数据',
      hint: '工作空间 · 导入',
      icon: Icons.folder_outlined,
      page: DataPage(),
    ),
    (
      label: '用量',
      hint: 'token · 花费 · 配额',
      icon: Icons.receipt_long_outlined,
      page: UsagePage(),
    ),
    // 排在「关于」前面：它是**偏好**，与上面几项同类；
    // 「关于」是这一列里唯一一个不改任何东西的，压在最后
    (
      label: '外观',
      hint: '主题 · 动效',
      icon: Icons.palette_outlined,
      page: AppearancePage(),
    ),
    (
      label: '关于',
      hint: '版本 · 更新',
      icon: Icons.info_outline_rounded,
      page: AboutPage(),
    ),
  ];

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final width = MediaQuery.sizeOf(context).width;

    return Scaffold(
      backgroundColor: theme.colorScheme.surface,
      body: SafeArea(
        child: Column(
          children: [
            _topBar(context),
            const Divider(height: 1),
            Expanded(
              // 窄屏（手机、被拖窄的窗口）放不下两列，退回一列 + 顶部下拉
              child: width < 640
                  ? _narrow(context)
                  : Row(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _nav(context),
                        const VerticalDivider(width: 1),
                        Expanded(child: _content(context)),
                      ],
                    ),
            ),
          ],
        ),
      ),
    );
  }

  /// 「← 返回」那一行。
  ///
  /// **返回而不是关闭**：这是一个页面，不是弹层 —— 用户是「进来了」，
  /// 那就该「回去」。一个 ✕ 在整屏页上读起来像「关掉这个应用」。
  Widget _topBar(BuildContext context) {
    final theme = Theme.of(context);
    return SizedBox(
      height: 48,
      child: Row(
        children: [
          const SizedBox(width: 6),
          TextButton.icon(
            onPressed: () => Navigator.of(context).maybePop(),
            icon: const Icon(Icons.arrow_back_rounded, size: 18),
            label: const Text('返回'),
            style: TextButton.styleFrom(
              foregroundColor: theme.colorScheme.onSurface,
            ),
          ),
          const SizedBox(width: 8),
          Text('设置', style: theme.textTheme.titleMedium),
        ],
      ),
    );
  }

  Widget _nav(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      width: 216,
      // 导航是一个**独立表面**（规范第二节）：两个功能区靠底色分开，
      // 比靠一条线分开省力
      color: theme.cortex.sidebar,
      child: ListView.builder(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 10),
        itemCount: _sections.length,
        itemBuilder: (context, i) {
          final s = _sections[i];
          final selected = i == _index;
          return Padding(
            padding: const EdgeInsets.only(bottom: 2),
            child: ListTile(
              dense: true,
              selected: selected,
              // **中性**（规范第九节）：「我现在在看哪一页」是位置，不是动作
              selectedTileColor: theme.cortex.sidebarAccent,
              // 连文字与图标一起收回中性 —— ListTile 的 `selectedColor`
              // 默认是 primary，只换底色的话那块紫只是挪到了前景
              selectedColor: theme.colorScheme.onSurface,
              shape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
              ),
              leading: Icon(s.icon, size: 20),
              title: Text(
                s.label,
                style: theme.textTheme.bodyMedium?.copyWith(
                  fontWeight: selected ? FontWeight.w600 : null,
                ),
              ),
              subtitle: Text(s.hint, style: theme.textTheme.labelSmall),
              onTap: () => setState(() => _index = i),
            ),
          );
        },
      ),
    );
  }

  Widget _content(BuildContext context) {
    final s = _sections[_index];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(28, 20, 28, 8),
          child: Text(s.label, style: Theme.of(context).textTheme.titleLarge),
        ),
        Expanded(
          child: Padding(
            // 整屏之后行长会失控 —— 正文超过一定宽度就难读了。
            // 内容整体封顶 1160 并**靠左**，不居中：居中的话左边会空出
            // 一大条，而导航就在那儿，读起来像两块不相干的东西
            padding: const EdgeInsets.fromLTRB(28, 0, 28, 20),
            child: Align(
              alignment: Alignment.topLeft,
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 1160),
                // 换页时把内部滚动位置也换掉：不给 key 的话，从一页很长的
                // 内容切到一页短的，滚动偏移会被沿用，看起来像那一页是空的
                child: KeyedSubtree(key: ValueKey(_index), child: s.page),
              ),
            ),
          ),
        ),
      ],
    );
  }

  Widget _narrow(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 10, 16, 6),
          child: DropdownButtonHideUnderline(
            child: DropdownButton<int>(
              value: _index,
              isExpanded: true,
              items: [
                for (var i = 0; i < _sections.length; i++)
                  DropdownMenuItem(
                    value: i,
                    child: Row(
                      children: [
                        Icon(_sections[i].icon, size: 18),
                        const SizedBox(width: 8),
                        Text(
                          _sections[i].label,
                          style: theme.textTheme.titleSmall,
                        ),
                      ],
                    ),
                  ),
              ],
              onChanged: (v) => setState(() => _index = v ?? 0),
            ),
          ),
        ),
        const Divider(height: 1),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
            child: KeyedSubtree(
              key: ValueKey(_index),
              child: _sections[_index].page,
            ),
          ),
        ),
      ],
    );
  }
}
