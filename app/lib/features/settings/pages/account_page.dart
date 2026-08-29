/// 「账号」这一页 —— 昵称、头像、口令，以及删号。
///
/// # 为什么值得一页
///
/// 改口令这条路**服务端 2026-08 就有了，而客户端一直没接** —— 于是设置里
/// 根本没有入口，用户要改密码只能去数据库。典型的「造好没人接」
/// （CLAUDE.md 约束 2 的反面：能力在、界面不说，等于没有）。
///
/// # 三条界面上的纪律
///
/// 1. **不可逆的动作要密码，且要说清后果。** 删号销毁整片 schema，所以它
///    有独立的确认对话框、要输密码、并明说「7 天后真删」。
/// 2. **改口令会把所有设备登出，包括这一台。** 不说的话用户会以为「改完
///    怎么被踢了」是 bug。
/// 3. **撤销删号的窗口只有 15 分钟，必须写出来。** 排期之后登录被拒，
///    手上那把 access token 一过期就再也撤不了 —— 用户以为「7 天内随时能
///    反悔」的话，代价是全部历史。
library;

import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/account.dart';
import '../../../state/app_providers.dart';
import '../../../state/auth_controller.dart';
import '../../../state/profile_controller.dart';
import '../widgets/settings_layout.dart';

class AccountPage extends ConsumerWidget {
  const AccountPage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final async = ref.watch(profileProvider);
    // ⚠️ **先看错误，再看加载中** —— 不能用 `.when()`。
    //
    // riverpod 在失败之后会停在 `AsyncLoading(error: …)`（带着错误的加载中），
    // 而 `.when()` 把它归进 loading 分支 —— 于是老服务端上用户看到的是一个
    // **永远转的圈**，而不是那句「这个部署没有账号体系」。
    // 写测试时撞到的：错误明明在 state 里，界面上却只有 spinner。
    if (async.error case final e?) return _Unavailable(error: e);
    if (async.value case final profile?) return _Body(profile: profile);
    return const Center(child: CircularProgressIndicator());
  }
}

class _Unavailable extends StatelessWidget {
  const _Unavailable({required this.error});
  final Object error;

  @override
  Widget build(BuildContext context) {
    final missing =
        error is CortexApiException &&
        ((error as CortexApiException).isMissing ||
            (error as CortexApiException).statusCode == 501);
    return ListView(
      padding: EdgeInsets.zero,
      children: [
        SettingsSection(
          description: missing
              ? '这个部署没有账号体系（用的是预共享 token，或者服务端版本较旧）—— '
                    '昵称、头像、口令都无从谈起。'
              : '读不到账号资料：$error',
          children: const [],
        ),
      ],
    );
  }
}

class _Body extends ConsumerStatefulWidget {
  const _Body({required this.profile});
  final Profile profile;

  @override
  ConsumerState<_Body> createState() => _BodyState();
}

class _BodyState extends ConsumerState<_Body> {
  late final TextEditingController _nickname = TextEditingController(
    text: widget.profile.nickname ?? '',
  );
  bool _savingNickname = false;

  @override
  void dispose() {
    _nickname.dispose();
    super.dispose();
  }

  /// 一句话反馈。**成功也要说** —— 改昵称之后界面上什么都没变（因为它本来
  /// 就显示着你刚输的那串），不说的话用户不知道存没存上。
  void _say(String message, {bool bad = false}) {
    if (!mounted) return;
    final scheme = Theme.of(context).colorScheme;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(message),
        backgroundColor: bad ? scheme.errorContainer : null,
      ),
    );
  }

  Future<void> _saveNickname() async {
    setState(() => _savingNickname = true);
    try {
      final raw = _nickname.text.trim();
      await ref
          .read(profileProvider.notifier)
          .setNickname(raw.isEmpty ? null : raw);
      _say(raw.isEmpty ? '昵称已清空，会显示登录名' : '昵称已保存');
    } on Object catch (e) {
      _say('$e', bad: true);
    } finally {
      if (mounted) setState(() => _savingNickname = false);
    }
  }

  Future<void> _pickAvatar() async {
    // 用仓库已有的 file_picker（附件那条路也是它），不新引一个依赖。
    // 只放这三种 —— 与服务端按魔数认的那三种一致。SVG 不在里面：它能带
    // 脚本，而头像会被当图片直接渲染
    final picked = await FilePicker.pickFiles(
      withData: true,
      dialogTitle: '选一张头像',
      type: FileType.custom,
      allowedExtensions: const ['png', 'jpg', 'jpeg', 'webp'],
    );
    final bytes = picked?.files.firstOrNull?.bytes;
    if (bytes == null) return;
    // 客户端先挡一次大小：等服务端回 400 的话，用户已经白等了一次上传
    if (bytes.length > 256 * 1024) {
      _say('头像最大 256 KiB，这张有 ${bytes.length ~/ 1024} KiB', bad: true);
      return;
    }
    try {
      await ref
          .read(profileProvider.notifier)
          .setAvatar(Uint8List.fromList(bytes));
      _say('头像已更新');
    } on Object catch (e) {
      _say('$e', bad: true);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final p = widget.profile;
    return ListView(
      padding: EdgeInsets.zero,
      children: [
        // ── 排期删除时，这条横幅压在最上面 ──
        //
        // 它是这一页最要紧的一句话：一个「我以为撤销了」的误会代价是全部历史
        if (p.purgeAfter case final due?) _PurgeBanner(due: due),
        SettingsSection(
          title: '资料',
          description: '昵称与头像。登录名不可改 —— 它是别人引用你的方式。',
          children: [
            SettingsCard(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const _AvatarView(size: 56),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          p.username,
                          style: theme.textTheme.bodyMedium?.copyWith(
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        const SizedBox(height: 2),
                        Text(
                          '登录名',
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: theme.cortex.foregroundTertiary,
                          ),
                        ),
                        const SizedBox(height: 10),
                        Wrap(
                          spacing: 8,
                          children: [
                            OutlinedButton.icon(
                              onPressed: _pickAvatar,
                              icon: const Icon(Icons.image_outlined, size: 16),
                              label: const Text('换头像'),
                            ),
                            if (p.hasAvatar)
                              TextButton(
                                onPressed: () async {
                                  try {
                                    await ref
                                        .read(profileProvider.notifier)
                                        .clearAvatar();
                                    _say('头像已移除');
                                  } on Object catch (e) {
                                    _say('$e', bad: true);
                                  }
                                },
                                child: const Text('移除'),
                              ),
                          ],
                        ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            SettingsCard(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  TextField(
                    controller: _nickname,
                    maxLength: 32,
                    decoration: const InputDecoration(
                      labelText: '昵称',
                      helperText: '留空则显示登录名',
                      isDense: true,
                    ),
                    onSubmitted: (_) => _saveNickname(),
                  ),
                  Align(
                    alignment: Alignment.centerRight,
                    child: FilledButton(
                      onPressed: _savingNickname ? null : _saveNickname,
                      child: Text(_savingNickname ? '保存中…' : '保存'),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
        SettingsSection(
          title: '口令',
          // 这句话必须在按钮**之前**：改完才发现被登出，会被当成 bug
          description:
              '改完口令，你在所有设备上的登录都会失效，包括这一台 —— '
              '需要重新登录一次。',
          children: [
            SettingsCard(
              child: Align(
                alignment: Alignment.centerLeft,
                child: OutlinedButton.icon(
                  onPressed: () => _showChangePassword(context, ref),
                  icon: const Icon(Icons.key_outlined, size: 16),
                  label: const Text('修改口令'),
                ),
              ),
            ),
          ],
        ),
        SettingsSection(
          title: '删除账号',
          description:
              '会销毁这个账号的全部数据：会话、消息、附件，一样不留。'
              '有 7 天冷静期，期间可以撤销。',
          children: [
            SettingsCard(
              child: Align(
                alignment: Alignment.centerLeft,
                child: OutlinedButton.icon(
                  onPressed: p.purgeAfter != null
                      ? null
                      : () => _showDeleteAccount(context, ref),
                  style: OutlinedButton.styleFrom(
                    foregroundColor: theme.colorScheme.error,
                  ),
                  icon: const Icon(Icons.delete_forever_outlined, size: 16),
                  label: Text(p.purgeAfter != null ? '已排期删除' : '删除账号…'),
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// 排期删除时那条横幅 —— 倒计时 + 撤销。
class _PurgeBanner extends ConsumerWidget {
  const _PurgeBanner({required this.due});
  final DateTime due;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final left = due.difference(DateTime.now());
    final days = left.inDays;
    final hours = left.inHours % 24;
    return Container(
      margin: const EdgeInsets.only(bottom: 14),
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            left.isNegative ? '这个账号随时会被删除' : '这个账号将在 $days 天 $hours 小时后被删除',
            style: theme.textTheme.bodyMedium?.copyWith(
              fontWeight: FontWeight.w700,
              color: theme.colorScheme.onErrorContainer,
            ),
          ),
          const SizedBox(height: 6),
          Text(
            // ⚠️ 这句话不能省：排期之后登录被拒，撤销只能靠手上这把还没过期的
            // access token（15 分钟）。以为「7 天内随时能反悔」的代价是全部历史
            '⚠ 现在就撤销 —— 一旦这次登录过期（约 15 分钟），你就再也登不进来，'
            '也就撤不回了，只能找管理员。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onErrorContainer,
              height: 1.5,
            ),
          ),
          const SizedBox(height: 10),
          FilledButton(
            onPressed: () async {
              final messenger = ScaffoldMessenger.of(context);
              try {
                await ref.read(profileProvider.notifier).cancelDeletion();
                messenger.showSnackBar(
                  const SnackBar(content: Text('已撤销 —— 账号保住了')),
                );
              } on Object catch (e) {
                messenger.showSnackBar(SnackBar(content: Text('$e')));
              }
            },
            child: const Text('撤销删除'),
          ),
        ],
      ),
    );
  }
}

/// 头像。没有就画首字母。
class _AvatarView extends ConsumerWidget {
  const _AvatarView({required this.size});
  final double size;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final bytes = ref.watch(avatarBytesProvider).value;
    final profile = ref.watch(profileProvider).value;
    if (bytes != null) {
      return ClipOval(
        child: Image.memory(
          bytes,
          width: size,
          height: size,
          fit: BoxFit.cover,
        ),
      );
    }
    final initial = (profile?.displayName ?? '?').characters.firstOrNull ?? '?';
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        shape: BoxShape.circle,
      ),
      child: Text(
        initial.toUpperCase(),
        style: theme.textTheme.titleLarge?.copyWith(
          color: theme.cortex.foregroundTertiary,
        ),
      ),
    );
  }
}

Future<void> _showChangePassword(BuildContext context, WidgetRef ref) async {
  final oldCtl = TextEditingController();
  final newCtl = TextEditingController();
  final again = TextEditingController();
  String? error;
  var busy = false;

  await showDialog<void>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setState) => AlertDialog(
        title: const Text('修改口令'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: oldCtl,
              obscureText: true,
              decoration: const InputDecoration(labelText: '当前口令'),
            ),
            TextField(
              controller: newCtl,
              obscureText: true,
              decoration: const InputDecoration(labelText: '新口令'),
            ),
            TextField(
              controller: again,
              obscureText: true,
              decoration: const InputDecoration(labelText: '再输一遍'),
            ),
            if (error != null)
              Padding(
                padding: const EdgeInsets.only(top: 10),
                child: Text(
                  error!,
                  style: TextStyle(color: Theme.of(ctx).colorScheme.error),
                ),
              ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: busy ? null : () => Navigator.of(ctx).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: busy
                ? null
                : () async {
                    // 两次不一致**在这里挡**，不发出去：服务端不知道用户
                    // 输了两遍，它只会照第一个改，而那多半不是用户想要的
                    if (newCtl.text != again.text) {
                      setState(() => error = '两次输入的新口令不一样');
                      return;
                    }
                    setState(() {
                      busy = true;
                      error = null;
                    });
                    try {
                      await ref
                          .read(cortexApiProvider)
                          .changePassword(oldCtl.text, newCtl.text);
                      if (ctx.mounted) Navigator.of(ctx).pop();
                      // 改完所有设备都被登出了 —— 把人送回登录页，
                      // 而不是让他在一个已经失效的界面里点来点去
                      await ref.read(authControllerProvider.notifier).signOut();
                    } on Object catch (e) {
                      setState(() {
                        busy = false;
                        error = '$e';
                      });
                    }
                  },
            child: Text(busy ? '提交中…' : '确定'),
          ),
        ],
      ),
    ),
  );
  oldCtl.dispose();
  newCtl.dispose();
  again.dispose();
}

Future<void> _showDeleteAccount(BuildContext context, WidgetRef ref) async {
  final pwd = TextEditingController();
  String? error;
  var busy = false;

  await showDialog<void>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setState) => AlertDialog(
        title: const Text('删除账号'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '这会销毁你的全部数据：会话、消息、附件，一样不留。\n\n'
              '有 7 天冷静期。但要注意：排期之后就登不进来了，'
              '撤销只能靠现在这次登录（约 15 分钟内）。',
            ),
            const SizedBox(height: 12),
            TextField(
              controller: pwd,
              obscureText: true,
              decoration: const InputDecoration(labelText: '输入口令以确认'),
            ),
            if (error != null)
              Padding(
                padding: const EdgeInsets.only(top: 10),
                child: Text(
                  error!,
                  style: TextStyle(color: Theme.of(ctx).colorScheme.error),
                ),
              ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: busy ? null : () => Navigator.of(ctx).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(ctx).colorScheme.error,
            ),
            onPressed: busy
                ? null
                : () async {
                    setState(() {
                      busy = true;
                      error = null;
                    });
                    try {
                      await ref
                          .read(profileProvider.notifier)
                          .requestDeletion(pwd.text);
                      if (ctx.mounted) Navigator.of(ctx).pop();
                    } on Object catch (e) {
                      setState(() {
                        busy = false;
                        error = '$e';
                      });
                    }
                  },
            child: Text(busy ? '提交中…' : '删除我的账号'),
          ),
        ],
      ),
    ),
  );
  pwd.dispose();
}
