import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import 'pages/about_page.dart';
import 'pages/appearance_page.dart';
import 'pages/notifications_page.dart';
import 'pages/permissions_page.dart';
import 'pages/computer_use_page.dart';
import 'pages/computer_use_section.dart';
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
/// **导航分四组**（模型 / 工具 / 偏好 / 系统，2026-08-24 起）。
/// 上一版写着「只有八项不分组」—— 现在十项了，每组两三条，组标题开始
/// 挣回它占的那一行：想改模型的人不必扫过技能和用量。
class SettingsPage extends ConsumerStatefulWidget {
  const SettingsPage({super.key});

  @override
  ConsumerState<SettingsPage> createState() => _SettingsPageState();
}

/// 导航的四组。声明顺序 = 展示顺序。
enum _NavGroup {
  model('模型'),
  tools('工具'),
  prefs('偏好'),
  system('系统');

  const _NavGroup(this.label);

  final String label;
}

/// 一页。
typedef _Section = ({
  String label,
  String hint,
  IconData icon,
  Widget page,
  _NavGroup group,

  /// 这一项在这个部署上摆不摆得出来。`null` = 恒摆。
  ///
  /// ⚠️ 闸放在**目录**上而不只是页面里：一个点进去一片空白的菜单项，
  /// 比没有这个菜单项更糟（CLAUDE.md 约束 2 在界面上的样子）。
  /// 而判据必须与那一页用的是**同一个** provider，各判各的迟早对不上。
  Provider<bool>? gate,
});

class _SettingsPageState extends ConsumerState<SettingsPage> {
  /// **全量索引**，不是「可见项里的第几个」。
  ///
  /// 差别在闸会变：`/health` 还没回来时电脑操作那一项不摆，回来之后摆上。
  /// 存可见序号的话，那一刻用户选中的页会**悄悄换成另一页** —— 而他什么
  /// 都没点。存全量索引则天然不受影响。
  int _index = 0;

  // 组内排序沿用原则「按我要改什么，不按它属于哪个模块」；
  // 组间顺序 = _NavGroup 声明顺序。列表按组连续排，组头在渲染时
  // 「与上一项不同组就画」—— 不用二级结构，闸掉一项也不会剩个空组头
  // （组里全被闸掉时组头跟着最后一项一起消失）
  static final _sections = <_Section>[
    (
      label: '模型服务',
      hint: '供应商 · key · 型号',
      icon: Icons.psychology_outlined,
      page: const ModelPage(),
      group: _NavGroup.model,
      gate: null,
    ),
    (
      label: '默认模型',
      hint: '主 · 快速 · 绘画',
      icon: Icons.tune_rounded,
      page: const ModelRolesPage(),
      group: _NavGroup.model,
      gate: null,
    ),
    // 技能紧挨着连接器：两者都是「模型手上多了什么」。
    // 差别是连接器给的是**能力**（去做一件它本来做不到的事），
    // 技能给的是**做法**（同一件事按你的规矩做）
    (
      label: '技能',
      hint: '写好的做法 · 按需取用',
      icon: Icons.auto_stories_outlined,
      page: const SkillsPage(),
      group: _NavGroup.tools,
      gate: null,
    ),
    (
      // 「连接器」而不是「MCP」：MCP 是协议名，说的是**怎么接**；
      // 用户在这一列里找的是**接什么**。协议名留在页内解释里
      label: '连接器',
      hint: '一键接入 · MCP server',
      icon: Icons.extension_outlined,
      page: const McpPage(),
      group: _NavGroup.tools,
      gate: null,
    ),
    // 它**不压在连接器页里面**（2026-08-23 搬出来的）。理由不是分类，
    // 是权限：别的连接器交出去的是一台别人写的 server，只有这一条交出去的
    // 是你这台机器的屏幕与键鼠
    (
      label: '电脑操作',
      hint: '截屏与键鼠 · 默认关',
      icon: Icons.desktop_windows_outlined,
      page: const ComputerUsePage(),
      group: _NavGroup.tools,
      gate: computerUseAvailableProvider,
    ),
    (
      label: '外观',
      hint: '主题 · 动效',
      icon: Icons.palette_outlined,
      page: const AppearancePage(),
      group: _NavGroup.prefs,
      gate: null,
    ),
    (
      label: '通知',
      hint: '提示音',
      icon: Icons.notifications_outlined,
      page: const NotificationsPage(),
      group: _NavGroup.prefs,
      gate: null,
    ),
    (
      label: '权限与沙箱',
      hint: '默认档 · 这台机器的保护',
      icon: Icons.shield_outlined,
      page: const PermissionsPage(),
      group: _NavGroup.prefs,
      gate: null,
    ),
    (
      label: '数据',
      hint: '工作空间 · 导入',
      icon: Icons.folder_outlined,
      page: const DataPage(),
      group: _NavGroup.prefs,
      gate: null,
    ),
    (
      label: '连接',
      hint: '数据源 · 后端状态',
      icon: Icons.cable_rounded,
      page: const ConnectionPage(),
      group: _NavGroup.system,
      gate: null,
    ),
    (
      label: '用量',
      hint: 'token · 花费 · 配额',
      icon: Icons.receipt_long_outlined,
      page: const UsagePage(),
      group: _NavGroup.system,
      gate: null,
    ),
    // 「关于」压在最后：它是这一列里唯一一个不改任何东西的
    (
      label: '关于',
      hint: '版本 · 更新',
      icon: Icons.info_outline_rounded,
      page: const AboutPage(),
      group: _NavGroup.system,
      gate: null,
    ),
  ];

  /// 这一刻摆得出来的那些**全量索引**（见 [_index] 上那段）。
  List<int> get _visible {
    final out = <int>[];
    for (var i = 0; i < _sections.length; i++) {
      final gate = _sections[i].gate;
      if (gate == null || ref.watch(gate)) out.add(i);
    }
    return out;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final width = MediaQuery.sizeOf(context).width;

    final visible = _visible;
    // **选中项落在一个已经不摆了的页上时，回落到第一页。**
    //
    // 这里算的是局部值而不是 `setState(_index = …)`：build 里改 state 是
    // 禁忌，而且也没必要 —— 闸再翻回来时 `_index` 还指着原来那一页，
    // 用户会回到他本来待着的地方。
    //
    // 不处理的后果不是「显示错了」，是**崩溃**：窄屏那条路的
    // `DropdownButton` 断言 value 必须在 items 里。
    final index = visible.contains(_index) ? _index : visible.first;

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
                  ? _narrow(context, visible, index)
                  : Row(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _nav(context, visible, index),
                        const VerticalDivider(width: 1),
                        Expanded(child: _content(context, index)),
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

  Widget _nav(BuildContext context, List<int> visible, int index) {
    final theme = Theme.of(context);
    // 表面用 Material 而不是 Container(color)：ListTile 的选中底色画在
    // **最近的 Material 祖先**上，中间隔一层着色的 ColoredBox 会把那块
    // 高亮整个盖掉 —— 表现是选中项看不出选中（测试里框架的
    // debug 断言抓到的，真机上就是那个样子）
    return Material(
      color: theme.cortex.sidebar,
      child: SizedBox(
        width: 216,
        child: ListView.builder(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 10),
          itemCount: visible.length,
          itemBuilder: (context, row) {
            final i = visible[row];
            final s = _sections[i];
            final selected = i == index;
            // 组头：与**可见的**上一项不同组才画。判据是可见序列而不是
            // 全量序列 —— 一组被闸得只剩没被闸的那项时组头还在；
            // 整组都被闸掉时组头跟着一起消失，不留一个空标题
            final newGroup =
                row == 0 || _sections[visible[row - 1]].group != s.group;
            final tile = Padding(
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
            if (!newGroup) return tile;
            return Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Padding(
                  // 首组顶部少留：上面已经有 ListView 自己的 padding
                  padding: EdgeInsets.fromLTRB(14, row == 0 ? 2 : 14, 14, 4),
                  child: Text(
                    s.group.label,
                    style: theme.textTheme.labelSmall?.copyWith(
                      fontWeight: FontWeight.w700,
                      letterSpacing: 0.8,
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ),
                ),
                tile,
              ],
            );
          },
        ),
      ),
    );
  }

  Widget _content(BuildContext context, int index) {
    final s = _sections[index];
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
                child: KeyedSubtree(key: ValueKey(index), child: s.page),
              ),
            ),
          ),
        ),
      ],
    );
  }

  Widget _narrow(BuildContext context, List<int> visible, int index) {
    final theme = Theme.of(context);
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 10, 16, 6),
          child: DropdownButtonHideUnderline(
            child: DropdownButton<int>(
              // ⚠️ 必须是 `index` 而不是 `_index`：DropdownButton 断言
              // value 出现在 items 里，选中项被闸掉时直接崩
              value: index,
              isExpanded: true,
              items: [
                for (final i in visible)
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
              key: ValueKey(index),
              child: _sections[index].page,
            ),
          ),
        ),
      ],
    );
  }
}
