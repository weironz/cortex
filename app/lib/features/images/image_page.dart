/// 图片 —— 一个输入框 + 一面「我的图片」。照 ChatGPT 的那一页。
///
/// # 为什么它不是一条会话
///
/// 生图与对话是两条协议：那条流式吐 token，这条一次回一张图，中间没有
/// 「上下文」这回事。做成会话的代价很具体 —— 撰写框那一套（附件、权限档、
/// 工作区）对生图一样都用不上，而生图要的（尺寸、张数）在那儿一个都没有。
///
/// # 灵感墙不做
///
/// ChatGPT 那一页顶上有一排「生成图片」的示例（动漫 / 水下 / 手写批注…）。
/// 那是内容运营 —— 有人在维护那份清单。我们没有，编几条塞进去只会是
/// 一面永远不变的假墙。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/generated_image.dart';
import '../../models/model_role.dart';
import '../../models/model_source.dart';
import '../../state/image_controller.dart';
import '../../state/model_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import '../settings/pages/model_picker.dart';
import '../settings/pages/model_roles_page.dart' show modelRolesProvider;
import 'widgets/image_spec.dart';
import 'widgets/image_thumb.dart';
import 'widgets/image_viewer.dart';

class ImagePage extends ConsumerStatefulWidget {
  const ImagePage({
    super.key,
    this.onToggleSessions,
    this.sessionsVisible = false,
  });

  /// 收起 / 展开左栏。窄屏上 `AppShell` 换成「开抽屉」。
  ///
  /// **画廊也要有这个按钮**：少了它，从这一页回会话列表在窄屏上就只剩
  /// 「从屏幕左缘划」——一个没有任何提示的手势。
  final VoidCallback? onToggleSessions;
  final bool sessionsVisible;

  @override
  ConsumerState<ImagePage> createState() => _ImagePageState();
}

class _ImagePageState extends ConsumerState<ImagePage> {
  final _prompt = TextEditingController();
  final _scroll = ScrollController();

  /// 这一次用哪个型号。`ModelPick()` = 交给服务端挑。
  ///
  /// **只管这一次**，不写回「绘画模型」那个指派 —— 与撰写框下面那个
  /// chip 同一条规矩：想改默认的人去设置里改，在这儿改的是当下这一张。
  ModelPick? _pick;

  String? _size;
  int _count = 1;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    _prompt.dispose();
    super.dispose();
  }

  void _onScroll() {
    // 离底还有一屏就开始取下一页 —— 等真的到底再取，用户会看到一段空白
    if (!_scroll.hasClients) return;
    final left = _scroll.position.maxScrollExtent - _scroll.position.pixels;
    if (left < 600) {
      unawaited(ref.read(imageControllerProvider.notifier).loadMore());
    }
  }

  /// 当前这一次实际会用哪一对 `(来源, 型号)`。
  ///
  /// 页面上没选过就用「绘画模型」那个指派 —— 那是用户在设置里表达过的
  /// 意愿，比一个空白更近。两个都没有就交给服务端挑。
  ModelPick get _effective {
    final chosen = _pick;
    if (chosen != null) return chosen;
    final role = ref.read(modelRolesProvider).value?.of(ModelRole.image);
    if (role == null) return const ModelPick();
    return ModelPick(source: role.source, model: role.model);
  }

  /// 规格面板要知道的两件事：尺寸算不算数、一次能不能出多张。
  ///
  /// # ⚠️ 「自动」那一档不能什么都不说
  ///
  /// 第一版只在**指名了型号**时算这两位，选「自动」时两句提示一句都不出。
  /// 而「自动」是默认档，用户唯一那条能画图的来源又恰恰是中转站 ——
  /// 于是最常见的那条路上，尺寸静默地只是提示词里的一句话，界面却
  /// 一个字都没提。「没指名」不等于「没有事实可说」。
  ///
  /// 指名了就按那条来源算；没指名就看**所有可能被挑中的来源**，
  /// 按最坏情况说：
  ///
  /// * 任一候选是自定义端点 → 尺寸可能不兑现，要提醒
  /// * 并非所有候选都能一次出多张 → 数量可能是连发凑的，要提醒
  ///
  /// 宁可多提醒一次，也不要在真会发生的那条路上闭嘴。
  ({bool nativeMultiImage, bool customEndpoint}) _spec(ModelPick pick) {
    final all = ref.watch(modelSourcesProvider).value?.sources ?? const [];
    bool native(ModelSource s) => s.provider == 'alibaba' && !s.isCustom;

    final id = pick.source;
    if (id != null) {
      final s = all.where((s) => s.id == id).firstOrNull;
      // 指名了却查不到那条来源（刚被删掉）—— 按未知处理，走下面的最坏情况
      if (s != null) {
        return (nativeMultiImage: native(s), customEndpoint: s.isCustom);
      }
    }

    // 候选 = 开着的、且**真有能生图的型号**的那些来源。
    // 不筛「有生图型号」的话，一堆纯对话的来源会把结论带偏
    final catalog = ref.watch(modelCatalogProvider).value;
    final drawable = <String>{
      for (final m in catalog?.models ?? const [])
        if (m.imageOutput == true) m.source,
    };
    final candidates = all
        .where((s) => s.enabled && drawable.contains(s.id))
        .toList();
    if (candidates.isEmpty) {
      // 一条都认不出来时**别下断言**：两句提示都不画，而不是画一个
      // 猜出来的结论。这一格下面那个模型选择器会说清「一个候选都没有」
      return (nativeMultiImage: false, customEndpoint: false);
    }
    return (
      nativeMultiImage: candidates.every(native),
      customEndpoint: candidates.any((s) => s.isCustom),
    );
  }

  @override
  Widget build(BuildContext context) {
    final state = ref.watch(imageControllerProvider);
    final pick = _effective;
    final spec = _spec(pick);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '图片',
          subtitle: '描述一张图，它画出来',
          leading: widget.onToggleSessions == null
              ? null
              : IconButton(
                  onPressed: widget.onToggleSessions,
                  iconSize: 19,
                  tooltip: widget.sessionsVisible ? '隐藏会话栏' : '显示会话栏',
                  icon: Icon(
                    widget.sessionsVisible
                        ? Icons.menu_open_rounded
                        : Icons.menu_rounded,
                  ),
                ),
        ),
        Expanded(
          child: ListView(
            controller: _scroll,
            padding: const EdgeInsets.fromLTRB(20, 16, 20, 24),
            children: [
              Center(
                child: ConstrainedBox(
                  // 输入框跟着窗口无限变宽的话，一行提示词会横跨整个屏幕 ——
                  // 与对话那边同一个理由，正文有一个可读的宽度上限
                  constraints: const BoxConstraints(maxWidth: 760),
                  child: _composer(context, state, pick, spec),
                ),
              ),
              const SizedBox(height: 28),
              _wall(context, state),
            ],
          ),
        ),
      ],
    );
  }

  Widget _composer(
    BuildContext context,
    ImageState state,
    ModelPick pick,
    ({bool nativeMultiImage, bool customEndpoint}) spec,
  ) {
    final theme = Theme.of(context);
    final busy = state.generating;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        TextField(
          controller: _prompt,
          enabled: !busy,
          minLines: 1,
          maxLines: 5,
          textInputAction: TextInputAction.newline,
          decoration: InputDecoration(
            hintText: '描述新图片…',
            border: const OutlineInputBorder(),
            suffixIcon: IconButton(
              tooltip: '生成',
              onPressed: busy ? null : _generate,
              icon: busy
                  ? const SizedBox(
                      width: 16,
                      height: 16,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.arrow_upward_rounded),
            ),
          ),
          onSubmitted: busy ? null : (_) => _generate(),
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            _ChipButton(
              key: const ValueKey('chip:model'),
              icon: Icons.brush_outlined,
              label: pick.model ?? '自动挑最便宜的',
              onTap: busy ? null : _pickModel,
            ),
            const SizedBox(width: 8),
            _ChipButton(
              key: const ValueKey('chip:spec'),
              icon: Icons.tune_rounded,
              label: '$_count · ${_sizeLabel()}',
              onTap: busy ? null : () => _pickSpec(spec),
            ),
          ],
        ),
        if (busy) ...[
          const SizedBox(height: 10),
          // 生图要几十秒。**必须说它在干活，还要说大概多久** ——
          // 一个转圈图标在第二十秒时读起来就是「卡住了」
          Text(
            _count > 1 ? '正在画 $_count 张，通常要一两分钟…' : '正在画，通常要几十秒…',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
        ],
        if (state.error != null) ...[
          const SizedBox(height: 10),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Text(
                  '${state.error}',
                  key: const ValueKey('images:error'),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ),
              // ⚠️ **这条错误必须有一条出路。**
              //
              // 画廊只在页面建起来时拉一次。首次那一下失败（2026-08-22
              // 实测：app 启动时正撞上 nginx 重启，连接被掐），这条红字
              // 就一直挂着，而唯一的办法是重启整个应用 —— 一个红字加一个
              // 死胡同，比没有这条错误更让人恼火
              TextButton(
                key: const ValueKey('images:retry'),
                onPressed: state.loading || state.generating
                    ? null
                    : () =>
                          ref.read(imageControllerProvider.notifier).refresh(),
                child: const Text('重试'),
              ),
            ],
          ),
        ],
      ],
    );
  }

  String _sizeLabel() =>
      kImageSizes.where((s) => s.value == _size).firstOrNull?.label ?? '自动';

  Widget _wall(BuildContext context, ImageState state) {
    final theme = Theme.of(context);

    if (state.unsupported) {
      return const EmptyState(
        icon: Icons.image_not_supported_outlined,
        title: '这个后端没有画廊',
        // 说清是**后端**旧了，不是出错了 —— 重试永远不会成功
        description: '它是一个老版本的部署，还没有 /images 这条路。升级之后这里会自己出现。',
      );
    }
    if (state.loading) {
      return const Padding(
        padding: EdgeInsets.only(top: 40),
        child: Center(child: CircularProgressIndicator()),
      );
    }
    if (state.items.isEmpty) {
      return const EmptyState(
        icon: Icons.image_outlined,
        title: '还没有画过图',
        description: '上面描述一张图，画出来的都会留在这儿 —— 在对话里让它画的也算。',
      );
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('我的图片', style: theme.textTheme.titleSmall),
        const SizedBox(height: 10),
        LayoutBuilder(
          builder: (context, c) {
            // 列数按可用宽度算（每格最窄 160），不写死 —— 与卡片墙同一路数
            final columns = (c.maxWidth / 180).floor().clamp(2, 8);
            return GridView.builder(
              shrinkWrap: true,
              physics: const NeverScrollableScrollPhysics(),
              gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                crossAxisCount: columns,
                crossAxisSpacing: 10,
                mainAxisSpacing: 10,
              ),
              itemCount: state.items.length,
              itemBuilder: (context, i) {
                final img = state.items[i];
                return ImageThumb(
                  key: ValueKey('thumb:${img.id}'),
                  image: img,
                  onTap: () => _open(img),
                );
              },
            );
          },
        ),
        if (state.loadingMore)
          const Padding(
            padding: EdgeInsets.only(top: 16),
            child: Center(
              child: SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
            ),
          ),
      ],
    );
  }

  Future<void> _pickModel() async {
    // **全仓库只此一份选择器**（`model_picker.dart`），参数与「默认模型」
    // 那一页的绘画那一行逐字相同 —— 两处不一致的表现是同一个型号在一处
    // 选得到、另一处选不到
    final picked = await pickModel(
      context,
      ref,
      current: _effective,
      headline: '绘画模型 · 只管这一次',
      firstTitle: '自动挑最便宜的',
      firstSubtitle: '在所有能生图的来源里挑最便宜的那个',
      // 「自动」那一档存的是聊天用的 `kAutoModel`，生图这条路不认它
      allowAuto: false,
      where: (m) => m.imageOutput == true,
      // 生图模型基本都不支持工具调用，开着这条会把每一项都画成灰的
      requireTools: false,
    );
    if (picked == null || !mounted) return;
    setState(() => _pick = picked.pick);
  }

  Future<void> _pickSpec(
    ({bool nativeMultiImage, bool customEndpoint}) spec,
  ) async {
    final got = await showImageSpecSheet(
      context,
      size: _size,
      count: _count,
      // 判据与服务端 `Protocol::native_n` 同源：只有走 DashScope 原生
      //（alibaba 且没填自定义端点）那一条一次能出多张。「自动」档怎么算，
      // 见 [_spec]
      nativeMultiImage: spec.nativeMultiImage,
      customEndpoint: spec.customEndpoint,
    );
    if (got == null || !mounted) return;
    setState(() {
      _size = got.size;
      _count = got.count;
    });
  }

  Future<void> _generate() async {
    final text = _prompt.text.trim();
    if (text.isEmpty) return;
    final pick = _effective;
    final ok = await ref
        .read(imageControllerProvider.notifier)
        .generate(
          prompt: text,
          model: pick.model,
          source: pick.source,
          size: _size,
          n: _count,
        );
    // **画成了才清空。** 失败时留着，用户不用重打一遍 ——
    // 而失败恰恰是最需要再试一次的时候
    if (ok && mounted) _prompt.clear();
  }

  Future<void> _open(GeneratedImage img) async {
    final again = await showImageViewer(context, img);
    if (again == null || !mounted) return;
    // 「以此为提示词重画」：把那句话填回输入框并聚焦，**不直接开画** ——
    // 这个动作的意义就是让他改一改，替他按下去等于跳过了唯一有价值的一步
    setState(() {
      _prompt.text = again;
      _prompt.selection = TextSelection.collapsed(offset: again.length);
    });
  }
}

/// 输入框底下那两个小按钮。
class _ChipButton extends StatelessWidget {
  const _ChipButton({
    super.key,
    required this.icon,
    required this.label,
    required this.onTap,
  });

  final IconData icon;
  final String label;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Material(
      color: theme.cortex.sidebarAccent,
      borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(icon, size: 14, color: theme.cortex.foregroundTertiary),
              const SizedBox(width: 6),
              ConstrainedBox(
                // 型号名可以很长（`gemini-3-pro-image-preview`），
                // 不封顶的话这一行会把两个 chip 挤出屏幕
                constraints: const BoxConstraints(maxWidth: 220),
                child: Text(
                  label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall,
                ),
              ),
              const SizedBox(width: 2),
              Icon(
                Icons.expand_more_rounded,
                size: 14,
                color: theme.cortex.foregroundTertiary,
              ),
            ],
          ),
        ),
      ),
    );
  }
}
