/// 图片 —— 一条**会话** + 一面「我的图片」。照 ChatGPT 的那一页。
///
/// # 它就是对话，只是话题是画图
///
/// 早先这里是一个表单，直接打 `/llm/image`。用户看完的第一句话是
/// 「图片生成应该是对话式的，和标准对话一样的逻辑，只是识别到要生成图片，
/// 就调用图片生成模型而已」—— 而那正是 `generate_image` 工具本来就在做的事。
///
/// 所以这一页现在**复用 `ConversationView` 与 `MessageComposer`**，
/// 与聊天页逐字相同的那一套。差别只有三处：
/// 它记住的是自己那条会话（[MainViewNotifier] 里的 `_imageSession`）、
/// 型号 chip 选的是绘画角色、以及底下多一面图库。
///
/// 不复用的代价很具体：气泡、附件、确认弹层、重发草稿会各走各的，
/// 而「在对话里能，在图片页不能」这种差别用户根本分不清是两个功能。
///
/// # 灵感墙不做
///
/// ChatGPT 那一页顶上有一排「生成图片」的示例（动漫 / 水下 / 手写批注…）。
/// 那是内容运营 —— 有人在维护那份清单。我们没有，编几条塞进去只会是
/// 一面永远不变的假墙。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/generated_image.dart';
import '../../models/image_prefs.dart';
import '../../models/model_role.dart';
import '../../models/model_source.dart';
import '../../state/image_controller.dart';
import '../../state/model_controller.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../state/composer_draft.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import '../chat/widgets/confirm_panel.dart';
import '../chat/widgets/conversation_view.dart';
import '../chat/widgets/message_composer.dart';
import '../settings/pages/model_picker.dart';
import '../settings/pages/model_roles_page.dart' show modelRolesProvider;
import 'widgets/album_bar.dart';
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
  final _scroll = ScrollController();

  /// 这一次用哪个型号。`ModelPick()` = 交给服务端挑。
  ///
  /// **只管这一次**，不写回「绘画模型」那个指派 —— 与撰写框下面那个
  /// chip 同一条规矩：想改默认的人去设置里改，在这儿改的是当下这一张。
  ModelPick? _pick;

  /// 正在看图库（而不是对话）。见头上那个开关的注释。
  bool _gallery = false;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
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
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );
    // 「还没开口」的判据与 `ChatPane` 逐字相同 —— 两处差一个条件就是
    // 「落地页与对话同时出现」，而那种错位只有真跑起来才看得见
    final empty = ref.watch(
      chatControllerProvider.select(
        (s) =>
            s.activeTranscript.isEmpty &&
            !s.isStreamingActive &&
            !(s.activeTranscriptState?.loading ?? false),
      ),
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '图片',
          subtitle: empty ? '描述一张图，它画出来' : null,
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
          actions: [
            // ⚠️ **图库要有一个常驻入口。**
            //
            // 第一版没有：图库只画在空会话的落地页上，开口之后就让位给
            // 对话（照 ChatGPT）。用户报上来的第一句话是「图片库怎么
            // 找不到了」—— 因为回去的唯一办法是「新对话」，而没人会把
            // 「新建」理解成「回到我的图片」。
            //
            // 图库是一个**地方**，不是某个形态的附属品。所以它有自己的
            // 开关，任何时候都按得到。
            TextButton.icon(
              key: const ValueKey('images:gallery'),
              onPressed: () => setState(() => _gallery = !_gallery),
              icon: Icon(
                _gallery
                    ? Icons.chat_bubble_outline_rounded
                    : Icons.grid_view_rounded,
                size: 16,
              ),
              label: Text(_gallery ? '回到对话' : '我的图片'),
            ),
            const SizedBox(width: 4),
            TextButton.icon(
              key: const ValueKey('images:new'),
              onPressed: streaming
                  ? null
                  : () {
                      ref.read(mainViewProvider.notifier).newImageSession();
                      // 开新对话就该看见对话，不该停在图库上 ——
                      // 否则「新对话」这个按钮点了像没反应
                      setState(() => _gallery = false);
                    },
              icon: const Icon(Icons.add_rounded, size: 16),
              label: const Text('新对话'),
            ),
          ],
        ),
        // 「我的图片」被按下 —— 整页就是图库，不带输入框：
        // 这是一个**翻东西**的地方，不是一个写提示词的地方。
        if (_gallery)
          Expanded(
            child: CustomScrollView(
              controller: _scroll,
              slivers: _wall(context, state),
            ),
          )
        // ⚠️ **另外两个形态与 `ChatPane` 是同一个判据。**
        //
        // 空会话 = 落地页：输入框 + 「我的图片」。开口之后图库让位给对话
        // —— 照 ChatGPT。把图库常驻在对话下面试过想过，代价是两个都要滚，
        // 而 `ConversationView` 自带滚动与自动跟随，套进外层滚动里就是
        // 「竖直方向无界约束」那个当场空白的老坑。
        else if (empty)
          Expanded(
            child: CustomScrollView(
              controller: _scroll,
              slivers: [
                SliverToBoxAdapter(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(20, 16, 20, 28),
                    child: Center(
                      child: ConstrainedBox(
                        // 输入框跟着窗口无限变宽的话，一行提示词会横跨整个屏幕
                        constraints: const BoxConstraints(maxWidth: 760),
                        child: _composer(context, state, pick, spec, sessionId),
                      ),
                    ),
                  ),
                ),
                ..._wall(context, state),
              ],
            ),
          )
        else ...[
          const Expanded(child: ConversationView()),
          Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 760),
              child: _composer(context, state, pick, spec, sessionId),
            ),
          ),
        ],
      ],
    );
  }

  Widget _composer(
    BuildContext context,
    ImageState state,
    ModelPick pick,
    ({bool nativeMultiImage, bool customEndpoint}) spec,
    String? sessionId,
  ) {
    final theme = Theme.of(context);
    final prefs = ref.watch(imagePrefsProvider);
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );
    final busy = streaming;

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // 与 `ChatPane` 共用这一摞。确认弹层挂在这儿而不是别处：一轮正在跑
        // 时用户的眼睛就在输入框上，离发送键近到划不过去
        const ConfirmPanel(),
        MessageComposer(
          enabled: sessionId != null,
          sessionId: sessionId,
          // 图片页自己管会话（`app_providers` 里那条恒非空的图片会话），
          // 所以这里走不到白纸那条路 —— 真走到了也只能说明上面那个
          // `enabled` 已经把输入框关掉了，兑现一条会话反而更糟
          ensureSession: () =>
              sessionId ??
              ref.read(chatControllerProvider.notifier).materializeSession(),
          streaming: streaming,
          onSend: (text, attachments) => ref
              .read(chatControllerProvider.notifier)
              .send(text, attachments: attachments),
          onStop: ref.read(chatControllerProvider.notifier).stopGeneration,
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
              label: '${prefs.count} · ${_sizeLabel(prefs.size)}',
              onTap: busy ? null : () => _pickSpec(spec),
            ),
          ],
        ),
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

  String _sizeLabel(String? size) =>
      kImageSizes.where((s) => s.value == size).firstOrNull?.label ?? '自动';

  /// 图库那一面 —— **一串 sliver，不是一个 widget**。
  ///
  /// # ⚠️ 为什么非改不可
  ///
  /// 从前它是 `GridView(shrinkWrap: true, physics: NeverScrollable)` 套在
  /// 外层 `ListView` 里。`shrinkWrap` 会**把整页格子全部布局出来** ——
  /// 于是一页 24 张图**同时开下**，包括屏幕外那十几张。
  ///
  /// 生成出来的 PNG 实测 1.6–2.7 MB 一张，24 张就是 **50 MB 左右**。
  /// 在本机回环上完全看不出来，走公网就是几十秒的白屏加一堆流量。
  ///
  /// 换成 `SliverGrid` 之后按需建：只有滚到的那几格才取字节。
  List<Widget> _wall(BuildContext context, ImageState state) {
    Widget box(Widget child) => SliverToBoxAdapter(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
        child: child,
      ),
    );

    if (state.unsupported) {
      return [
        box(
          const EmptyState(
            icon: Icons.image_not_supported_outlined,
            title: '这个后端没有画廊',
            // 说清是**后端**旧了，不是出错了 —— 重试永远不会成功
            description: '它是一个老版本的部署，还没有 /images 这条路。升级之后这里会自己出现。',
          ),
        ),
      ];
    }
    if (state.loading) {
      return [
        box(
          const Padding(
            padding: EdgeInsets.only(top: 40),
            child: Center(child: CircularProgressIndicator()),
          ),
        ),
      ];
    }
    // 空相册与空图库要说**不一样**的话：在一个空相册里看到「还没有画过图」，
    // 用户会以为自己的图全没了
    if (state.items.isEmpty) {
      if (state.album != null) {
        return [
          box(AlbumBar(said: _say)),
          box(
            const EmptyState(
              icon: Icons.photo_album_outlined,
              title: '这个相册还是空的',
              description: '回「全部」里勾几张，点「加入相册」。',
            ),
          ),
        ];
      }
      return [
        box(
          const EmptyState(
            icon: Icons.image_outlined,
            title: '还没有画过图',
            description: '上面描述一张图，画出来的都会留在这儿 —— 在对话里让它画的也算。',
          ),
        ),
      ];
    }

    final selecting = state.selected.isNotEmpty;
    return [
      SliverToBoxAdapter(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 10),
          child: AlbumBar(said: _say),
        ),
      ),
      SliverPadding(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
        sliver: SliverGrid.builder(
          // 每格最窄 260 → 常见宽度上一行 3–4 格，格子落在 260–330。
          // 从前是 160，一屏能塞八列 —— 而 180 宽的格子上，一张图画得
          // 对不对根本看不出来，图库就退化成一面色块墙
          gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
            maxCrossAxisExtent: 330,
            crossAxisSpacing: 10,
            mainAxisSpacing: 10,
          ),
          itemCount: state.items.length,
          itemBuilder: (context, i) {
            final img = state.items[i];
            return ImageThumb(
              key: ValueKey('thumb:${img.id}'),
              image: img,
              selected: state.selected.contains(img.id),
              selecting: selecting,
              onToggleSelect: () => ref
                  .read(imageControllerProvider.notifier)
                  .toggleSelect(img.id),
              onSaid: _say,
              onChanged: () =>
                  ref.read(imageControllerProvider.notifier).refresh(),
              onTap: () {
                // Ctrl / ⌘ + 点 = 勾选。桌面上这是**通用**的多选手势，
                // 而长按在鼠标下面根本不是一个动作
                if (_multiSelectHeld) {
                  ref
                      .read(imageControllerProvider.notifier)
                      .toggleSelect(img.id);
                  return;
                }
                _open(img);
              },
            );
          },
        ),
      ),
      if (state.loadingMore)
        box(
          const Center(
            child: SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
        ),
    ];
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
    final prefs = ref.read(imagePrefsProvider);
    final got = await showImageSpecSheet(
      context,
      size: prefs.size,
      count: prefs.count,
      // 判据与服务端 `Protocol::native_n` 同源：只有走 DashScope 原生
      //（alibaba 且没填自定义端点）那一条一次能出多张。「自动」档怎么算，
      // 见 [_spec]
      nativeMultiImage: spec.nativeMultiImage,
      customEndpoint: spec.customEndpoint,
    );
    if (got == null || !mounted) return;
    // ⚠️ **写进 provider，不留本地副本。**
    //
    // `ChatController.send` 在发出去那一刻读 `imagePrefsProvider`。这边再
    // 存一份 `_size`/`_count` 的话，就是同一件事两个来源 —— 而不一致的
    // 表现是「chip 上写着 2 张，画出来 1 张」，且两边都不报错。
    ref
        .read(imagePrefsProvider.notifier)
        .set(ImagePrefs(size: got.size, count: got.count));
  }

  /// 此刻按着 Ctrl / ⌘ 吗。
  ///
  /// `InkWell.onTap` 不带修饰键，而为了拿到它去换一套 `Listener` +
  /// 自绘水波纹，代价远大于问一句键盘状态。
  bool get _multiSelectHeld {
    // 不能是 `const`：`LogicalKeyboardKey` 自己重载了 `==`
    final keys = {
      LogicalKeyboardKey.controlLeft,
      LogicalKeyboardKey.controlRight,
      LogicalKeyboardKey.metaLeft,
      LogicalKeyboardKey.metaRight,
    };
    return HardwareKeyboard.instance.logicalKeysPressed.any(keys.contains);
  }

  void _say(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      // 一条一条地排队的话，连点几个动作要等上十几秒才看到最后那条
      ..clearSnackBars()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _open(GeneratedImage img) async {
    final again = await showImageViewer(
      context,
      ViewerImage.fromGallery(img),
      onChanged: () => ref.read(imageControllerProvider.notifier).refresh(),
    );
    if (again == null || !mounted) return;
    // 「以此为提示词重画」：把那句话放进输入框，**不直接开画** ——
    // 这个动作的意义就是让他改一改，替他按下去等于跳过了唯一有价值的一步。
    //
    // 走 `composerDraftProvider` 而不是自己持一个 controller：输入框现在是
    // 共用的 `MessageComposer`，那条草稿通道本来就是给「改一改再发一次」
    // 造的（消息气泡里的重发用的也是它）
    ref.read(composerDraftProvider.notifier).offer(again);
    // 图库在落地页上，而重画要在对话里发 —— 但此刻会话可能已经有内容了。
    // 什么都不用做：草稿会落进哪个形态的输入框都一样，它只有一个
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
