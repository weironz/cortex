import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

/// Schemes a link in assistant output is allowed to use.
///
/// The text comes from a model, which in turn may be repeating something it
/// read out of a file or a retrieved memory — i.e. it is not trusted input. A
/// `file:`, `smb:` or custom-scheme URL would be handed straight to the OS
/// handler, so the set is closed rather than open: anything unlisted is shown
/// to the user instead of being launched.
const _allowedSchemes = {'http', 'https', 'mailto'};

/// Opens a link from rendered Markdown in the system browser.
///
/// Returns silently on success; failures surface as a snack bar rather than
/// nothing at all — a link that does nothing when clicked is worse than one
/// that says why.
Future<void> openExternalLink(BuildContext context, String raw) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  final url = raw.trim();
  final uri = Uri.tryParse(url);

  if (uri == null || !_allowedSchemes.contains(uri.scheme.toLowerCase())) {
    _complain(messenger, '只允许打开 http / https / mailto 链接：$url');
    return;
  }

  bool opened;
  try {
    opened = await launchUrl(uri, mode: LaunchMode.externalApplication);
  } on Object catch (e) {
    _complain(messenger, '打开链接失败：$e');
    return;
  }
  if (!opened) _complain(messenger, '系统没有能打开这个链接的程序：$url');
}

void _complain(ScaffoldMessengerState? messenger, String message) {
  messenger?.showSnackBar(
    SnackBar(content: Text(message), behavior: SnackBarBehavior.floating),
  );
}
