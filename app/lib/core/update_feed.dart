/// 「最新版本是哪个、从哪下、校验和在哪」—— 只有 HTTP 与 JSON，没有
/// `dart:io`，所以这一半在任何平台上都编译得过、也测得了。
///
/// 真正需要平台能力的是下载落盘、算哈希、拉起安装程序，那些在 `updater.dart`
/// 那道 seam 后面。
///
/// # 为什么版本来源是 GitHub Releases
///
/// 产物本来就只托管在那里，没有第二个下载源。让「谁说有新版本」与
/// 「能下到什么」出自同一处，就不存在「提示了一个下不到的版本」这种状态。
/// 自建分发可以用 `--dart-define=CORTEX_UPDATE_FEED=` 换掉整个地址。
library;

import 'dart:convert';

import 'package:http/http.dart' as http;

import '../models/json.dart';

/// 一次可安装的发布。
class UpdateRelease {
  const UpdateRelease({
    required this.version,
    required this.setupUrl,
    required this.setupName,
    required this.sumsUrl,
    this.notesUrl,
  });

  /// 形如 `0.1.8`（`v` 已剥掉）。
  final String version;

  /// Windows 安装程序的下载地址。
  final String setupUrl;

  /// 安装程序的文件名 —— **要拿它去 `SHA256SUMS` 里找对应那一行**。
  final String setupName;

  /// `SHA256SUMS` 的下载地址。
  final String sumsUrl;

  /// release 页面，给「看看这一版改了什么」用。
  final String? notesUrl;
}

/// 从 GitHub 的 release JSON 里挑出我们要的三样。
///
/// 缺任何一样都返回 null 而不是抛异常：一个只发了 Linux 产物、或者
/// 忘了传 `SHA256SUMS` 的 release，对桌面端来说就等于「没有可装的东西」。
/// 那种情况下**什么都不提示**才是对的 —— 提示一个装不了的版本，
/// 正是 roadmap 里写的「比没有更糟」。
UpdateRelease? parseLatestRelease(Object? json) {
  if (json is! Map<String, dynamic>) return null;

  final tag = asString(json['tag_name']).trim();
  if (tag.isEmpty) return null;
  final version = (tag.startsWith('v') || tag.startsWith('V'))
      ? tag.substring(1)
      : tag;
  if (version.isEmpty) return null;

  String? setupUrl;
  String? setupName;
  String? sumsUrl;
  for (final a in asObjectList(json['assets'])) {
    final name = asString(a['name']);
    final url = asString(a['browser_download_url']);
    if (url.isEmpty) continue;
    if (name == 'SHA256SUMS') {
      sumsUrl = url;
    } else if (name.endsWith('-setup.exe') && name.contains('windows')) {
      // 名字形如 cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe。
      // 同时看后缀与 `windows`：将来加了别的平台的安装器，光看 `-setup.exe`
      // 会把 mac 的那个也认成 Windows 的
      setupUrl = url;
      setupName = name;
    }
  }
  if (setupUrl == null || setupName == null || sumsUrl == null) return null;

  final notes = asStringOrNull(json['html_url']);
  return UpdateRelease(
    version: version,
    setupUrl: setupUrl,
    setupName: setupName,
    sumsUrl: sumsUrl,
    notesUrl: (notes == null || notes.isEmpty) ? null : notes,
  );
}

/// 拉一次 feed。失败一律抛 [UpdateException]，调用方按「这次没查到」处理。
///
/// 超时给得比普通请求短：这是一件**后台的、没人等的**事，卡住它不该拖住
/// 任何东西，而查不到的代价只是晚一天再查。
Future<UpdateRelease?> fetchLatestRelease(
  http.Client client,
  String feedUrl, {
  Duration timeout = const Duration(seconds: 10),
}) async {
  final Uri uri;
  try {
    uri = Uri.parse(feedUrl);
  } on FormatException {
    throw UpdateException('更新源地址不合法：$feedUrl');
  }

  final http.Response res;
  try {
    res = await client
        .get(
          uri,
          // GitHub 要求带 User-Agent，不带会 403；Accept 固定 API 版本，
          // 免得哪天默认版本变了把 assets 的字段名换掉
          headers: const {
            'accept': 'application/vnd.github+json',
            'user-agent': 'cortex-desktop',
          },
        )
        .timeout(timeout);
  } on Object catch (e) {
    throw UpdateException('连不上更新源：$e');
  }

  if (res.statusCode == 404) return null; // 一个 release 都还没发过
  if (res.statusCode != 200) {
    throw UpdateException('更新源返回 HTTP ${res.statusCode}');
  }

  try {
    return parseLatestRelease(jsonDecode(utf8.decode(res.bodyBytes)));
  } on FormatException catch (e) {
    throw UpdateException('更新源返回的不是 JSON：${e.message}');
  }
}

/// 从 `sha256sum` 的输出里取 [fileName] 那一行的哈希。
///
/// 格式是 `<64 位十六进制><两个空格><文件名>`（GNU coreutils）。找不到那一行
/// 返回 null —— 调用方必须把它当**校验失败**，而不是「那就不校验了」。
/// 这份文件存在的唯一理由就是安装包没有代码签名。
String? sha256For(String sumsText, String fileName) {
  for (final line in const LineSplitter().convert(sumsText)) {
    final t = line.trim();
    if (t.isEmpty) continue;
    // 二进制模式的 sha256sum 会在文件名前多一个 `*`
    final m = RegExp(r'^([0-9a-fA-F]{64})\s+\*?(.+)$').firstMatch(t);
    if (m == null) continue;
    if (m.group(2)!.trim() == fileName) return m.group(1)!.toLowerCase();
  }
  return null;
}

class UpdateException implements Exception {
  const UpdateException(this.message);
  final String message;
  @override
  String toString() => message;
}
