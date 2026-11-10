/// 「操作这台电脑」那一节 —— 内容本体，壳在 [`computer_use_page.dart`]。
///
/// # 为什么从连接器页搬走了
///
/// 原先它压在连接器页最下面，理由是「回答的是同一个问题：模型手上有什么」。
/// 那个归类没错，但它把**权限最大的一组能力**藏在了一页的末尾 ——
/// 要往下滚过一整列 MCP server 才看得见。别的都是「接一台别人写的 server」，
/// 只有这一条交出去的是你这台机器的屏幕与键鼠；它该有自己的一页。
///
/// 内容留在这个文件里没动，页面只是个壳 —— 这样搬家不碰那段措辞。
///
/// # ⚠️ 做不到的时候**整节不画**
///
/// 做不到有两种情形：跑在容器里（云端会话，没有屏幕），或者这个构建没编进
/// 那一组（Linux 桌面）。两种都不是「关着」而是「没有」——
/// 摆一个打开也没用的开关，比没有这个开关更糟（CLAUDE.md 约束 2）。
/// 判据来自 `/health` 的 `computer_use`，是 agent 自己报的。
///
/// # 开关旁边那段话不是免责声明
///
/// 用户要做的是一个**知情**的决定，而做这个决定需要知道三件他不会自己想到
/// 的事：观察范围只覆盖 allowlist 里的应用窗口，窗口内容会交给模型，以及
/// 点击或输入落在真实的桌面上。所以那段话写的是「它能看见什么、能做什么」，
/// 不是「风险自负」。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:file_picker/file_picker.dart';

import '../../../core/theme.dart';
import '../../../models/health_status.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';
import '../widgets/settings_layout.dart';
import 'computer_input_recovery_section.dart';

/// 这个部署做不做得到「操作电脑」。
///
/// # ⚠️ 抽成 provider 是为了**只有一个判据**
///
/// 设置目录里的那一项与这一页的内容各判各的话，会长出两种都很难看的状态：
/// 菜单里有这一项、点进去一片空白；或者反过来，能力在但没有入口。
/// 这正是这个仓库的头号故障形状（造好了没人调用）的近亲 —— 判据分叉。
///
/// 所以它有且只有这一处定义，[`ComputerUseSection`] 与
/// `settings_sheet.dart` 的菜单闸都读它。
final computerUseAvailableProvider = Provider<bool>(
  (ref) => ref.watch(healthProvider).value?.canObserveComputer == true,
);

/// 这一页是否应该出现在**当前桌面端**。
///
/// `computerUseAvailableProvider` 回答“驱动现在可用吗”；这里回答“用户
/// 有没有地方修复/配置它”。本机 agent 即使因为 allowlist 为空或 bundled
/// driver 尚未就绪而暂时不可用，也必须保留设置入口；云端 cortexd 仍然
/// 不出现这一页。
final computerUseSettingsAvailableProvider = Provider<bool>(
  (ref) => ref.watch(healthProvider).value?.isLocalAgent == true,
);

/// 这个部署是否真的能投递鼠标键盘动作。
///
/// C1 的 Cua 链路只开放 list/observe。界面必须据细粒度能力换文案，不能把
/// “可以观察”写成“可以操作”，否则用户会以为 agent 能替自己点按钮。
final computerActAvailableProvider = Provider<bool>(
  (ref) => ref.watch(healthProvider).value?.canActOnComputer == true,
);

/// Injectable file chooser so widget tests do not depend on a native dialog.
final computerUseFilePickerProvider = Provider<Future<List<String>> Function()>(
  (ref) {
    return () async {
      final result = await FilePicker.pickFiles(
        type: FileType.custom,
        allowedExtensions: const ['exe'],
        allowMultiple: true,
        dialogTitle: '选择允许 Cortex 操作的应用',
      );
      return [
        for (final file in result?.files ?? const <PlatformFile>[])
          if (file.path case final path? when path.trim().isNotEmpty) path,
      ];
    };
  },
);

class ComputerUseSection extends ConsumerWidget {
  const ComputerUseSection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);

    if (!ref.watch(computerUseSettingsAvailableProvider)) {
      return const SizedBox.shrink();
    }

    final available = ref.watch(computerUseAvailableProvider);
    final on = ref.watch(computerUseProvider);
    final capabilities = ref.watch(healthProvider).value?.computerCapabilities;
    final canAct = ref.watch(computerActAvailableProvider);
    final writeReadiness = capabilities?.writeReadiness;
    final writeBlocked =
        writeReadiness != null &&
        writeReadiness != ComputerWriteReadiness.ready;
    final canDesktopAct = canAct && capabilities?.listWindows == true;
    final canBrowser = capabilities?.browser == true;
    final canBrowserAct = canAct && canBrowser;
    final allowlist = ref.watch(computerUseAllowlistProvider);
    final packagedAllowlist = ref.watch(computerPackagedAllowlistProvider);
    final browserAllowlist = ref.watch(computerBrowserAllowlistProvider);
    final backgroundInput = capabilities?.backgroundInput ?? false;
    final vision = ref.watch(selectedModelVisionProvider);
    final effective = ref.watch(computerUseEffectiveProvider);
    final accessibilityOnly =
        available && capabilities?.accessibilityOnly == true;
    final blocked = on && available && !effective;
    final modelVisionNote = vision == false ? '当前模型明确不支持视觉输入' : '当前模型视觉能力未知';

    return SettingsSection(
      title: '操作这台电脑',
      description: writeBlocked
          ? available
                ? '写入前置状态未就绪，当前仍可观察允许的窗口。'
                : '写入前置状态未就绪；当前也未报告可用的观察能力。'
          : !available
          ? '本机 Computer Use 驱动尚未就绪。配置允许的应用后，本地 agent '
                '会自动重启并重新检测；云端会话始终不会获得宿主桌面权限。'
          : canDesktopAct
          ? 'Windows 本机可操作 allowlist 内的桌面窗口；未配置站点时不启动隔离浏览器。'
          : canBrowserAct
          ? '当前可使用隔离浏览器；网页访问和动作受站点 allowlist 与逐次审批限制。'
          : canAct
          ? '本机 agent 支持输入动作；具体范围以报告的窗口和浏览器能力为准。'
          : '当前版本只开放观察，不会替用户点击或输入。',
      children: [
        if (writeReadiness == ComputerWriteReadiness.authorityUnresolved) ...[
          const ComputerInputRecoverySection(),
          const SizedBox(height: 12),
        ],
        SettingsCard(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text('允许操作电脑', style: theme.textTheme.bodyMedium),
                        const SizedBox(height: 3),
                        Text(
                          switch (on) {
                            true when !available => '已保存开启偏好，但本机驱动尚未就绪。',
                            true when blocked =>
                              '已开启，但$modelVisionNote，且后端未确认支持无视觉结构化通道；'
                                  '这一轮不会加载 Computer Use 工具。',
                            true when canDesktopAct =>
                              backgroundInput
                                  ? '已开启。动作优先在后台投递，并逐次经过权限与审批。'
                                  : '已开启。动作逐次经过权限与审批。',
                            true when canBrowserAct =>
                              '已允许隔离浏览器。网页动作逐次经过站点边界与动作审批。',
                            true when canAct =>
                              '已开启。后端按报告的能力提供观察和动作，并逐次经过权限与审批。',
                            true => '已开启只读。允许窗口的界面元素可被观察，但不会发送鼠标或键盘动作。',
                            _ when canAct => '已关闭。模型不会观察或操作已允许的电脑能力。',
                            _ => '已关闭。模型不会观察任何允许窗口。',
                          },
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: blocked
                                ? theme.colorScheme.error
                                : theme.cortex.foregroundTertiary,
                          ),
                        ),
                      ],
                    ),
                  ),
                  Switch(
                    key: const ValueKey('computer_use:toggle'),
                    value: available && on,
                    onChanged: !available
                        ? null
                        : (v) =>
                              ref.read(computerUseProvider.notifier).set(on: v),
                  ),
                ],
              ),
              if (blocked) ...[
                const SizedBox(height: 12),
                SettingsNote(
                  key: const ValueKey('computer_use:vision-error'),
                  icon: Icons.visibility_off_outlined,
                  tone: theme.colorScheme.error,
                  child: Text(
                    '$modelVisionNote，后端也未确认支持无视觉结构化通道。'
                    '请升级支持该通道的本地 agent，或选择已知支持视觉的模型；'
                    '当前保留开启偏好，但不会请求这组工具。',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: theme.colorScheme.error,
                      height: 1.5,
                    ),
                  ),
                ),
              ],
              if (on && available && effective) ...[
                const SizedBox(height: 12),
                SettingsNote(
                  key: const ValueKey('computer_use:observation-mode'),
                  icon: Icons.visibility_outlined,
                  child: Text(switch (vision) {
                    false =>
                      '当前模型明确不支持视觉输入；后端已确认无视觉结构化通道。'
                          '无视觉观察仅返回界面结构，不返回图片；已开放的浏览器工具也遵循这一限制。'
                          '如允许动作，也只能引用已观察到的元素，不允许图像或坐标后备。',
                    null =>
                      '当前模型视觉能力未知；后端已确认无视觉结构化通道，可以加载 Computer Use 工具。'
                          '本轮观察方式由后端按实际使用的模型确认；'
                          '只有确认支持视觉才启用图像与坐标，否则仅返回界面结构。',
                    true =>
                      '模型目录标为支持视觉；只有后端确认实际使用的模型支持视觉，才启用图像与坐标。'
                          '${accessibilityOnly ? '后端也支持无像素的结构化后备。' : '后端未确认支持无视觉结构化通道，不能保证无像素后备。'}',
                  }, style: theme.textTheme.labelSmall?.copyWith(height: 1.5)),
                ),
              ],
              if (accessibilityOnly) ...[
                const SizedBox(height: 12),
                const SettingsNote(
                  key: ValueKey('computer_use:accessibility-boundary'),
                  icon: Icons.pan_tool_outlined,
                  child: Text(
                    '无视觉时，目标的界面结构（AX）不完整就停止并交回给你处理（handoff），'
                    '不尝试截图或坐标后备。结构化通道不会增加输入或浏览器权限。',
                  ),
                ),
              ],
              if (!available) ...[
                const SizedBox(height: 12),
                const SettingsNote(
                  key: ValueKey('computer_use:not-ready'),
                  icon: Icons.settings_suggest_outlined,
                  child: Text(
                    '请先添加至少一个本机应用并确认安装包带有 Cua Driver。'
                    '修改列表会重启本地 agent；页面随后按新的 /health 自动更新。',
                  ),
                ),
              ],
              const SizedBox(height: 12),
              _CapabilitySummary(
                capabilities: capabilities,
                canAct: canAct,
                canObserve: available,
              ),
              const SizedBox(height: 12),
              _AllowlistEditor(entries: allowlist),
              const SizedBox(height: 12),
              _PackagedAllowlistEditor(entries: packagedAllowlist),
              const SizedBox(height: 12),
              _BrowserAllowlistEditor(entries: browserAllowlist),
              const SizedBox(height: 12),
              // ⚠️ 下面几句是这个开关的全部前提。它们讲的是用户**不会自己想到**
              // 的边界，而不是免责声明。
              _Point(
                icon: Icons.window_outlined,
                text:
                    '每次观察绑定一个 allowlist 内的应用窗口；界面元素会交给模型，'
                    '画面仅在后端确认实际模型支持视觉时才提供。'
                    '未列入 allowlist 的应用不会出现在窗口列表里。',
              ),
              const SizedBox(height: 8),
              if (canDesktopAct)
                _Point(
                  icon: Icons.mouse_outlined,
                  text:
                      '点击和输入落在真实的桌面窗口上，不在沙箱里；'
                      '每个动作执行后都会重新观察窗口，确认结果。',
                )
              else if (!canAct)
                _Point(
                  icon: Icons.visibility_outlined,
                  text: '当前只开放窗口与界面元素观察，不能点击、输入、发送或修改应用。',
                ),
              if (canBrowser) ...[
                const SizedBox(height: 8),
                _Point(
                  icon: Icons.public_rounded,
                  text:
                      '网页使用一次性隔离浏览器，不复用日常 Chrome/Edge profile；'
                      '所有连接受下方站点 allowlist 限制。'
                      '${canAct ? '每次只执行一个动作；单击和填写仅支持可验证的原生控件，按键和其他组件需由你操作。' : '当前只开放网页结构观察，不会执行网页动作。'}',
                ),
              ],
              const SizedBox(height: 8),
              _Point(
                icon: Icons.pan_tool_outlined,
                text:
                    '密码和凭据输入、付款确认、系统权限与 UAC 弹窗属于 handoff 场景：'
                    '自动化会停止，交回给你处理。',
              ),
              if (canAct) ...[
                const SizedBox(height: 8),
                _Point(
                  icon: Icons.shield_outlined,
                  text: '权限档留在「每次询问」，就能逐次看见它要观察或操作哪个允许窗口。',
                ),
              ],
              const SizedBox(height: 8),
              _Point(
                icon: Icons.cloud_off_outlined,
                text:
                    '本页只反映本机 agent 当前报告的能力；云端容器不会获得操控宿主桌面的权限。'
                    '观察结果与动作结果回到对应工具卡片；输入框上方的运行条显示当前动作，并提供既有停止键。',
              ),
            ],
          ),
        ),
      ],
    );
  }
}

class _AllowlistEditor extends ConsumerWidget {
  const _AllowlistEditor({required this.entries});

  final List<String> entries;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Material(
      color: scheme.surfaceContainerHigh,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        side: BorderSide(color: scheme.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        '允许的应用',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurface,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                      const SizedBox(height: 3),
                      Text(
                        '按 exe 的完整路径绑定。更换、删除或保存这个列表后，'
                        '本地 agent 会重启并重新验证文件身份。',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                          height: 1.45,
                        ),
                      ),
                    ],
                  ),
                ),
                TextButton(
                  key: const ValueKey('computer_use:allowlist-reset'),
                  onPressed: () => ref
                      .read(computerUseAllowlistProvider.notifier)
                      .resetToDefaults(),
                  child: const Text('恢复默认'),
                ),
              ],
            ),
            const SizedBox(height: 8),
            if (entries.isEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 8),
                child: Text(
                  '还没有允许的应用。添加至少一个 .exe 后，下一位本地 agent '
                  '才能启动受边界约束的窗口驱动。',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.error,
                    height: 1.45,
                  ),
                ),
              )
            else
              for (final entry in entries) _ExecutableTile(entry: entry),
            const SizedBox(height: 4),
            OutlinedButton.icon(
              key: const ValueKey('computer_use:allowlist-add'),
              onPressed: () async {
                try {
                  final selected = await ref.read(
                    computerUseFilePickerProvider,
                  )();
                  if (selected.isEmpty) return;
                  final notifier = ref.read(
                    computerUseAllowlistProvider.notifier,
                  );
                  for (final path in selected) {
                    await notifier.add(path);
                  }
                } on Object catch (error) {
                  if (!context.mounted) return;
                  ScaffoldMessenger.of(
                    context,
                  ).showSnackBar(SnackBar(content: Text('选择应用失败：$error')));
                }
              },
              icon: const Icon(Icons.add_rounded, size: 18),
              label: const Text('添加应用'),
            ),
          ],
        ),
      ),
    );
  }
}

class _ExecutableTile extends ConsumerWidget {
  const _ExecutableTile({required this.entry});

  final String entry;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final name = entry.replaceAll('\\', '/').split('/').last;
    return ListTile(
      key: ValueKey('computer_use:allowlist:$entry'),
      dense: true,
      contentPadding: EdgeInsets.zero,
      leading: Icon(
        Icons.terminal_outlined,
        size: 18,
        color: scheme.onSurfaceVariant,
      ),
      title: Text(name, style: theme.textTheme.labelMedium),
      subtitle: Text(
        entry,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: theme.textTheme.labelSmall?.copyWith(
          color: scheme.onSurfaceVariant,
        ),
      ),
      trailing: IconButton(
        tooltip: '从 allowlist 移除',
        onPressed: () =>
            ref.read(computerUseAllowlistProvider.notifier).remove(entry),
        icon: const Icon(Icons.close_rounded, size: 18),
      ),
    );
  }
}

class _PackagedAllowlistEditor extends ConsumerStatefulWidget {
  const _PackagedAllowlistEditor({required this.entries});

  final List<String> entries;

  @override
  ConsumerState<_PackagedAllowlistEditor> createState() =>
      _PackagedAllowlistEditorState();
}

class _PackagedAllowlistEditorState
    extends ConsumerState<_PackagedAllowlistEditor> {
  final _identity = TextEditingController();
  final _executable = TextEditingController();
  String _kind = 'aumid';
  String? _error;

  @override
  void dispose() {
    _identity.dispose();
    _executable.dispose();
    super.dispose();
  }

  Future<void> _add() async {
    final identity = _identity.text.trim();
    final executable = _executable.text.trim();
    final before = ref.read(computerPackagedAllowlistProvider);
    await ref
        .read(computerPackagedAllowlistProvider.notifier)
        .add(kind: _kind, identity: identity, executable: executable);
    final after = ref.read(computerPackagedAllowlistProvider);
    if (after.length == before.length &&
        !after.any(
          (entry) =>
              entry.toLowerCase().contains(identity.toLowerCase()) &&
              entry.toLowerCase().endsWith(executable.toLowerCase()),
        )) {
      setState(() {
        _error = _kind == 'aumid'
            ? 'AUMID 通常形如 PackageName_8wekyb3d8bbwe!App。'
            : 'Package family 不应包含 !App，并需要填写精确 exe 路径。';
      });
      return;
    }
    _identity.clear();
    _executable.clear();
    setState(() => _error = null);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Material(
      color: scheme.surfaceContainerHigh,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        side: BorderSide(color: scheme.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: ExpansionTile(
        key: const ValueKey('computer_use:packaged-editor'),
        tilePadding: const EdgeInsets.symmetric(horizontal: 12),
        childrenPadding: const EdgeInsets.fromLTRB(12, 0, 12, 12),
        title: Text(
          'Windows 打包应用',
          style: theme.textTheme.bodySmall?.copyWith(
            color: scheme.onSurface,
            fontWeight: FontWeight.w600,
          ),
        ),
        subtitle: Text(
          widget.entries.isEmpty
              ? '可选：UWP / MSIX 应用需要 AUMID 或 package family 身份。'
              : '${widget.entries.length} 个身份已绑定到精确 exe。',
          style: theme.textTheme.labelSmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
        children: [
          for (final entry in widget.entries)
            _PackagedIdentityTile(entry: entry),
          const SizedBox(height: 6),
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'aumid', label: Text('AUMID')),
              ButtonSegment(value: 'family', label: Text('Package family')),
            ],
            selected: {_kind},
            onSelectionChanged: (selection) {
              setState(() {
                _kind = selection.single;
                _error = null;
              });
            },
          ),
          const SizedBox(height: 8),
          TextField(
            key: const ValueKey('computer_use:packaged-identity'),
            controller: _identity,
            decoration: InputDecoration(
              isDense: true,
              labelText: _kind == 'aumid' ? 'AUMID' : 'Package family',
              hintText: _kind == 'aumid'
                  ? 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App'
                  : 'Microsoft.WindowsCalculator_8wekyb3d8bbwe',
            ),
          ),
          const SizedBox(height: 8),
          TextField(
            key: const ValueKey('computer_use:packaged-exe'),
            controller: _executable,
            decoration: InputDecoration(
              isDense: true,
              labelText: '精确 exe 路径',
              hintText: r'C:\Program Files\WindowsApps\...\App.exe',
              errorText: _error,
            ),
          ),
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerRight,
            child: FilledButton.tonal(
              key: const ValueKey('computer_use:packaged-add'),
              onPressed: _add,
              child: const Text('绑定打包应用'),
            ),
          ),
        ],
      ),
    );
  }
}

class _PackagedIdentityTile extends ConsumerWidget {
  const _PackagedIdentityTile({required this.entry});

  final String entry;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final parts = entry.split('|');
    final identity = parts.length > 1 ? parts[1] : entry;
    final executable = parts.last;
    return ListTile(
      key: ValueKey('computer_use:packaged:$entry'),
      contentPadding: EdgeInsets.zero,
      dense: true,
      title: Text(identity),
      subtitle: Text(executable, maxLines: 2, overflow: TextOverflow.ellipsis),
      trailing: IconButton(
        tooltip: '移除这个打包应用',
        onPressed: () =>
            ref.read(computerPackagedAllowlistProvider.notifier).remove(entry),
        icon: const Icon(Icons.close_rounded, size: 18),
      ),
    );
  }
}

class _BrowserAllowlistEditor extends ConsumerStatefulWidget {
  const _BrowserAllowlistEditor({required this.entries});

  final List<String> entries;

  @override
  ConsumerState<_BrowserAllowlistEditor> createState() =>
      _BrowserAllowlistEditorState();
}

class _BrowserAllowlistEditorState
    extends ConsumerState<_BrowserAllowlistEditor> {
  final _controller = TextEditingController();
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _add() async {
    final raw = _controller.text.trim();
    if (raw.isEmpty) return;
    final before = ref.read(computerBrowserAllowlistProvider);
    await ref.read(computerBrowserAllowlistProvider.notifier).add(raw);
    final after = ref.read(computerBrowserAllowlistProvider);
    if (after.length == before.length && !after.contains(raw.toLowerCase())) {
      setState(() {
        _error = '只接受公网域名，例如 example.com、*.example.com:443';
      });
      return;
    }
    _controller.clear();
    setState(() => _error = null);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '允许的网站',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurface,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 3),
          Text(
            '普通网页走一次性隔离浏览器，不复用你的 cookie 或登录态。'
            '列表为空时网页工具关闭；不会把空列表解释成“允许所有网站”。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
              height: 1.45,
            ),
          ),
          const SizedBox(height: 10),
          if (widget.entries.isEmpty)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: Text(
                '尚未允许任何网站，隔离浏览器当前不可用。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            )
          else
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: Wrap(
                spacing: 7,
                runSpacing: 7,
                children: [
                  for (final site in widget.entries)
                    InputChip(
                      key: ValueKey('computer_use:site:$site'),
                      label: Text(site),
                      onDeleted: () => ref
                          .read(computerBrowserAllowlistProvider.notifier)
                          .remove(site),
                    ),
                ],
              ),
            ),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: TextField(
                  key: const ValueKey('computer_use:site-input'),
                  controller: _controller,
                  decoration: InputDecoration(
                    isDense: true,
                    hintText: 'example.com 或 *.example.com:443',
                    errorText: _error,
                  ),
                  textInputAction: TextInputAction.done,
                  onSubmitted: (_) => _add(),
                ),
              ),
              const SizedBox(width: 8),
              FilledButton.tonal(
                key: const ValueKey('computer_use:site-add'),
                onPressed: _add,
                child: const Text('添加'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// agent 自己报告的细粒度能力。
///
/// 老 agent 没有这份数据时不能把缺字段画成「不可用」——「没报」和「明确不支持」
/// 是两件事。这里会把两种状态分开，避免旧客户端被一份能力矩阵误伤。
class _CapabilitySummary extends StatelessWidget {
  const _CapabilitySummary({
    required this.capabilities,
    required this.canAct,
    required this.canObserve,
  });

  final ComputerCapabilities? capabilities;
  final bool canAct;
  final bool canObserve;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final capabilities = this.capabilities;
    final readiness = capabilities?.writeReadiness;
    final writeBlocked =
        readiness != null && readiness != ComputerWriteReadiness.ready;
    // 旧 host 继续展示原始细项；有显式 readiness 后，写入细项也必须满足 act。
    final writeDetailsAvailable = readiness == null || canAct;
    final readinessNote = switch (readiness) {
      null => '写入初始化状态未报告；沿用旧版动作能力声明。',
      ComputerWriteReadiness.ready => '写入前置状态已就绪；输入动作仍取决于驱动能力和既有权限。',
      ComputerWriteReadiness.authorityMissing => '写入尚未初始化，当前禁止点击或输入。',
      ComputerWriteReadiness.provisionIncomplete => '写入初始化尚未完成，当前禁止点击或输入。',
      ComputerWriteReadiness.ownerMismatch => '写入记录属于其他用户，当前用户不能操作。',
      ComputerWriteReadiness.authorityUnresolved => '写入状态未决，当前禁止点击或输入。',
      ComputerWriteReadiness.authorityInvalid => '写入记录无效或已损坏，当前禁止点击或输入。',
      ComputerWriteReadiness.authorityBusy => '写入状态被其他进程占用，当前禁止点击或输入。',
      ComputerWriteReadiness.probeUnavailable => '暂时无法检查写入状态，当前禁止点击或输入。',
      ComputerWriteReadiness.unsupported => '当前宿主不支持电脑输入动作。',
      ComputerWriteReadiness.unknown => '无法识别写入状态，当前禁止点击或输入。',
    };

    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '本机 agent 报告的能力',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurface,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 3),
          Text(
            '状态来自 /health；本页不会把未报告的细项猜成可用。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
          const SizedBox(height: 9),
          Text(
            '$readinessNote${writeBlocked && canObserve ? '仍可观察已允许的窗口。' : ''}',
            key: const ValueKey('computer_use:write-readiness'),
            style: theme.textTheme.labelSmall?.copyWith(
              color: readiness == ComputerWriteReadiness.authorityInvalid
                  ? scheme.error
                  : writeBlocked
                  ? theme.cortex.warning
                  : theme.cortex.foregroundTertiary,
              height: 1.5,
            ),
          ),
          const SizedBox(height: 9),
          if (capabilities == null)
            Text(
              '对端是旧版 agent，没有报告窗口列表、后台输入等细项；'
              '仅能确认整体观察和输入状态，不能据此启用无视觉结构化通道。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
                height: 1.5,
              ),
            )
          else
            Wrap(
              spacing: 6,
              runSpacing: 6,
              children: [
                _CapabilityPill(
                  label: '应用列表',
                  available: capabilities.listApps,
                ),
                _CapabilityPill(
                  label: '窗口列表',
                  available: capabilities.listWindows,
                ),
                _CapabilityPill(label: '界面观察', available: capabilities.observe),
                _CapabilityPill(
                  label: '无视觉结构化通道',
                  available: capabilities.accessibilityOnly,
                ),
                _CapabilityPill(label: '输入动作', available: canAct),
                _CapabilityPill(
                  label: '后台输入',
                  available:
                      writeDetailsAvailable && capabilities.backgroundInput,
                ),
                _CapabilityPill(
                  label: '敏感字段检测',
                  available: capabilities.secureInputDetection,
                ),
                _CapabilityPill(
                  label: '浏览器自动化',
                  available: writeDetailsAvailable && capabilities.browser,
                ),
              ],
            ),
        ],
      ),
    );
  }
}

class _CapabilityPill extends StatelessWidget {
  const _CapabilityPill({required this.label, required this.available});

  final String label;
  final bool available;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final foreground = available
        ? scheme.onSurface
        : theme.cortex.foregroundTertiary;

    return Semantics(
      label: '$label：${available ? '可用' : '不可用'}',
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
        decoration: BoxDecoration(
          color: available ? theme.cortex.accentSoft : Colors.transparent,
          borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
          border: Border.all(color: scheme.outlineVariant),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              available
                  ? Icons.check_circle_outline_rounded
                  : Icons.remove_circle_outline_rounded,
              size: 13,
              color: available ? theme.cortex.success : foreground,
            ),
            const SizedBox(width: 5),
            Text(
              '$label：${available ? '可用' : '不可用'}',
              style: theme.textTheme.labelSmall?.copyWith(color: foreground),
            ),
          ],
        ),
      ),
    );
  }
}

class _Point extends StatelessWidget {
  const _Point({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: 14, color: theme.cortex.foregroundTertiary),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            text,
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
              height: 1.5,
            ),
          ),
        ),
      ],
    );
  }
}
