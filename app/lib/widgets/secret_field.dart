import 'package:flutter/material.dart';

import '../core/theme.dart';

/// 三态密钥输入框：**未设 / 已设但不显示（掩码）/ 正在改**。
///
/// # 为什么这必须是一个共享控件，而不是各页各写
///
/// 「已存的密钥」「有默认值的空输入」是三态，而空输入框只能表达第一态
/// 「未设」—— 这就是质量回顾 2026-08-25 根因 B 里「已存密钥像空的」「默认
/// 值不可见」两个病例的病理：各页各写，写漏一个态就是一个病例。约定见
/// `state/async_gate.dart`；这三态只许在这里长一次，页面拿参数说话。
///
/// # 三态的呈现
///
/// 1. **已设但不显示**（[storedTail] 非空、非编辑中）：画掩码
///    `●●●●●●●● + 尾四位` —— 「长得像已经填了」，而不是一个像没存上的空框。
///    圆点是**画出来的**，不是真密钥的长度（长度本身也是不该泄露的信息）；
///    尾四位是服务端唯一回的明文。点一下（或 Tab 聚焦回车）才进编辑态。
/// 2. **正在改**：普通密码框，**从空白开始重输** —— 界面手里从来只有尾
///    四位，没有「改几个字符」这回事；把掩码圆点当预填塞进框里，一次
///    不小心的保存就会把 `●●●●…` 当成密钥存进服务端。默认遮字、可点开
///    明文：这是一串粘进来的东西，而一次看不见的粘贴迟早会错一次。
///    丢焦点且什么都没输时退回掩码态 —— 点开又反悔的人不该留下一个
///    看起来「密钥被清空了」的空框。
/// 3. **未设**（[storedTail] 为 null 或空串，如免 key 的本机 ollama）：
///    直接是输入框，**不画掩码** —— 画了等于谎称存过一把 key。
///
/// # 禁用必有原因
///
/// `enabled: false` 时必须同时给 [disabledReason]，构造期断言直接拦。
/// 理由是画在框下面的可见文案，不是鼠标悬停才露面的 tooltip ——
/// 「为什么灰」的答案不该需要人去猜哪里能悬停。
class SecretField extends StatefulWidget {
  const SecretField({
    super.key,
    required this.controller,
    this.storedTail,
    this.onChanged,
    this.onEditingChanged,
    this.enabled = true,
    this.disabledReason,
    this.editingHint = '粘贴新密钥，留空 = 不换',
    this.unsetHint,
    this.fieldKey,
    this.maskedFieldKey,
    this.resetEpoch,
  }) : assert(
         enabled || disabledReason != null,
         '禁用必有原因：enabled: false 时必须给 disabledReason'
         '（约定见 state/async_gate.dart）',
       );

  /// 正在输入的那一份。由调用方持有 —— 保存时读的是它。
  final TextEditingController controller;

  /// 已存密钥的**尾四位**（服务端只回这个）。`null` 或空串 = 未设，
  /// 三态里的第三种。
  final String? storedTail;

  final ValueChanged<String>? onChanged;

  /// 进入 / 退出编辑态时各通知一次（`true` = 进入）。
  ///
  /// 仅供父级切换**说明文案**这类显示推导；编辑态的门后来在控件内部，
  /// 父级手里的这份镜像不是真相。
  final ValueChanged<bool>? onEditingChanged;

  final bool enabled;

  /// 禁用时的原因，画在框下面。`enabled: true` 时留空。
  final String? disabledReason;

  /// 编辑态的占位文 —— 已存过密钥时从空白重输的那句。
  final String editingHint;

  /// 未设态的占位文。`null` = 不画占位。
  final String? unsetHint;

  /// 输入态那个 `TextField` 的 key（如 `ValueKey('field:xxx')`）。
  ///
  /// **按名字找，不按位置**：这一页上的输入框数量一直在变，
  /// `byType(TextField).last` 会随着加一个框悄悄指到别处。
  final Key? fieldKey;

  /// 掩码态那个可点块的 key。
  final Key? maskedFieldKey;

  /// 外部要**收回编辑态**时换一个值（典型：保存落库之后）。
  ///
  /// 控件比对前后两帧的引用，变了就退回掩码/未设态 —— 存完留一个空的
  /// 编辑框会读成「刚存的密钥没了」。存过之后用它的人递增一个计数器即可。
  final Object? resetEpoch;

  @override
  State<SecretField> createState() => _SecretFieldState();
}

class _SecretFieldState extends State<SecretField> {
  bool _editing = false;
  bool _reveal = false;

  /// 编辑态的焦点。丢焦点且什么都没输时退回掩码态。
  final _focus = FocusNode();

  bool get _hasStored => widget.storedTail?.isNotEmpty ?? false;

  @override
  void initState() {
    super.initState();
    _focus.addListener(() {
      if (!_focus.hasFocus &&
          _editing &&
          widget.controller.text.trim().isEmpty &&
          mounted) {
        setState(() => _editing = false);
        widget.onEditingChanged?.call(false);
      }
    });
  }

  @override
  void didUpdateWidget(SecretField oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 外部收走编辑态（保存落库那一帧）：退回掩码/未设，遮字也归位。
    // 不用 setState —— 父级这一帧本来就在重建我们
    if (oldWidget.resetEpoch != widget.resetEpoch && _editing) {
      _editing = false;
      _reveal = false;
    }
  }

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  /// 从掩码态进入编辑态，并直接把光标放进去 ——
  /// 点了还要再点一次才能打字，是把一次动作拆成了两次。
  void _beginEdit() {
    setState(() => _editing = true);
    widget.onEditingChanged?.call(true);
    // 等编辑态那个 TextField 建出来再要焦点，不然焦点落在空处
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focus.requestFocus();
    });
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final masked = _hasStored && !_editing;

    final Widget field;
    if (!widget.enabled) {
      // 禁用不假装可编辑：掩码态也不再是「点一下」的 InkWell ——
      // 一个点了没反应的可点块，是在说一件它做不到的事
      field = InputDecorator(
        decoration: InputDecoration(
          isDense: true,
          border: const OutlineInputBorder(),
          enabled: false,
          hintText: _hasStored ? null : widget.unsetHint,
          suffixIcon: _hasStored
              ? const Icon(Icons.lock_outline_rounded, size: 18)
              : null,
        ),
        child: _hasStored
            ? Text(
                '●●●●●●●●${widget.storedTail}',
                style: theme.textTheme.bodyMedium,
              )
            : null,
      );
    } else if (masked) {
      field = InkWell(
        key: widget.maskedFieldKey,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        onTap: _beginEdit,
        // 键盘也进得来：Tab 到这里按回车 = 点击
        onFocusChange: (has) {
          if (has) _beginEdit();
        },
        child: InputDecorator(
          decoration: const InputDecoration(
            isDense: true,
            border: OutlineInputBorder(),
            suffixIcon: Icon(Icons.edit_outlined, size: 18),
          ),
          child: Text(
            '●●●●●●●●${widget.storedTail}',
            style: theme.textTheme.bodyMedium,
          ),
        ),
      );
    } else {
      field = TextField(
        key: widget.fieldKey,
        controller: widget.controller,
        focusNode: _focus,
        autocorrect: false,
        enableSuggestions: false,
        obscureText: !_reveal,
        enabled: widget.enabled,
        onChanged: widget.onChanged,
        decoration: InputDecoration(
          isDense: true,
          border: const OutlineInputBorder(),
          // 编辑态从空白开始重输 —— 界面永远拿不到明文
          hintText: _hasStored ? widget.editingHint : widget.unsetHint,
          suffixIcon: IconButton(
            onPressed: () => setState(() => _reveal = !_reveal),
            iconSize: 18,
            icon: Icon(
              _reveal
                  ? Icons.visibility_off_outlined
                  : Icons.visibility_outlined,
            ),
          ),
        ),
      );
    }

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        field,
        if (!widget.enabled)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              widget.disabledReason!,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
                height: 1.4,
              ),
            ),
          ),
      ],
    );
  }
}
