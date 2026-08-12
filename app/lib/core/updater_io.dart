import 'dart:async';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:http/http.dart' as http;

import 'update_feed.dart';

/// 桌面能给自己装新版本。理由见 `updater.dart` 那道 seam。
///
/// **只有 Windows**：安装这一步靠的是我们自己发的 Inno 安装包，而 release
/// 现在只出 Windows 产物。别的桌面平台上提示更新等于提示一个下不到的东西。
final bool kUpdaterSupported = Platform.isWindows;

/// 环境变量 `CORTEX_UPDATE_FEED`，压过编译期那个默认地址。
///
/// # 为什么要有一个**运行期**的口子
///
/// 编译期的 `--dart-define` 意味着「换个更新源」要重新构建一次整个桌面端。
/// 两处付不起这个代价：
///
/// - **验证**。整条链（下载→校验→装→重启）只有真机能证明，而每验一次就
///   重编一次，代价高到最后没人验 —— 而这正是 roadmap 里担心的那种
///   「做一半」的来源
/// - **自建分发**。内网环境连不上 GitHub，他们需要的是改一个环境变量，
///   不是自己编一份客户端
///
/// 空串按「没设」处理，不是按「设成了空」——这是本仓库数到第 6 次的形状。
String? feedOverride() {
  final v = Platform.environment['CORTEX_UPDATE_FEED']?.trim();
  return (v == null || v.isEmpty) ? null : v;
}

/// 下载安装包，比对 SHA-256，返回它落在哪。
///
/// # 校验不过就删掉，而且绝不执行
///
/// 这个安装包**没有代码签名**，`SHA256SUMS` 是用户（和我们自己）唯一能确认
/// 「手上这份就是发布页上那份」的东西。对不上意味着传输被截断、镜像被投毒、
/// 或者 release 被人换过 —— 三种里没有一种应该以「那就装吧」收场。
///
/// 删掉那个文件也是必须的：留一个校验失败的 exe 在 `%TEMP%` 里，
/// 等于给下一次「反正已经下好了」留了个坑。
///
/// [onProgress] 收到 0..1；总长度未知（服务端没给 Content-Length）时不回调，
/// 界面照旧转圈就好 —— 假装知道进度比不知道更糟。
Future<String> downloadAndVerify(
  http.Client client,
  UpdateRelease release, {
  void Function(double fraction)? onProgress,
  Duration timeout = const Duration(minutes: 10),
}) async {
  final dir = Directory(
    '${Directory.systemTemp.path}${Platform.pathSeparator}cortex-update',
  );
  try {
    if (!dir.existsSync()) dir.createSync(recursive: true);
  } on FileSystemException catch (e) {
    throw UpdateException('建不出下载目录：${e.message}');
  }

  final target = File(
    '${dir.path}${Platform.pathSeparator}${release.setupName}',
  );

  // 先把校验和拿到手，**再**下那个几十兆的包：`SHA256SUMS` 缺失是「这一版
  // 没法验证」，那就根本不该开始下载
  final String sumsText;
  try {
    final res = await client
        .get(Uri.parse(release.sumsUrl))
        .timeout(const Duration(seconds: 30));
    if (res.statusCode != 200) {
      throw UpdateException('取校验和失败：HTTP ${res.statusCode}');
    }
    sumsText = res.body;
  } on UpdateException {
    rethrow;
  } on Object catch (e) {
    throw UpdateException('取校验和失败：$e');
  }

  final expected = sha256For(sumsText, release.setupName);
  if (expected == null) {
    throw UpdateException(
      '校验和文件里没有 ${release.setupName} 这一行 —— 无法确认下载到的是不是发布的那份',
    );
  }

  await _download(client, release.setupUrl, target, onProgress, timeout);

  final String actual;
  try {
    // 流式喂给 sha256，不要 `readAsBytes`：安装包十几兆，一次性读进内存
    // 在这里没必要，而同样的写法将来遇到更大的产物就是一次 OOM
    final digest = await sha256.bind(target.openRead()).first;
    actual = digest.toString().toLowerCase();
  } on Object catch (e) {
    await _deleteQuietly(target);
    throw UpdateException('算哈希失败：$e');
  }

  if (actual != expected) {
    await _deleteQuietly(target);
    throw UpdateException(
      '下载到的安装包与发布的校验和对不上（期望 ${expected.substring(0, 12)}…，'
      '实际 ${actual.substring(0, 12)}…）。没有安装，文件已删除。',
    );
  }
  return target.path;
}

Future<void> _download(
  http.Client client,
  String url,
  File target,
  void Function(double)? onProgress,
  Duration timeout,
) async {
  IOSink? sink;
  try {
    final req = http.Request('GET', Uri.parse(url));
    final res = await client.send(req).timeout(timeout);
    if (res.statusCode != 200) {
      throw UpdateException('下载失败：HTTP ${res.statusCode}');
    }
    final total = res.contentLength ?? 0;
    var got = 0;
    sink = target.openWrite();
    await for (final chunk in res.stream) {
      sink.add(chunk);
      got += chunk.length;
      if (total > 0) onProgress?.call(got / total);
    }
    await sink.flush();
    await sink.close();
    sink = null;
  } on UpdateException {
    await sink?.close();
    await _deleteQuietly(target);
    rethrow;
  } on Object catch (e) {
    await sink?.close();
    // 半截文件必须删掉：它长得跟下完的一模一样，只是短一点，
    // 而下一次「文件已存在」的判断会把它当成下好了
    await _deleteQuietly(target);
    throw UpdateException('下载失败：$e');
  }
}

Future<void> _deleteQuietly(File f) async {
  try {
    if (f.existsSync()) await f.delete();
  } on FileSystemException {
    // 删不掉也不该盖住真正的失败原因
  }
}

/// 传给安装程序的开关。**每一条都是真机上撞出来的**，逐条理由见
/// [launchInstaller]。单独提出来是为了能被测试钉住：少任何一条的症状都
/// 只发生在用户机器上，而且都是「应用没了」这一类。
const List<String> installerArgs = [
  '/VERYSILENT',
  '/SUPPRESSMSGBOXES',
  '/CLOSEAPPLICATIONS',
  '/FORCECLOSEAPPLICATIONS',
  '/RESTARTAPPLICATIONS',
  '/NORESTART',
];

/// 拉起安装程序，然后**由调用方退出应用**。
///
/// # [installerArgs] 里每个开关各自在挡什么
///
/// - `/VERYSILENT`：无界面。用户点的是「更新」，不是「我想再走一遍安装向导」
/// - `/SUPPRESSMSGBOXES`：**真机上抓到的**。`/VERYSILENT` 只是不显示向导，
///   出错时它照样弹模态框并**停在那里等人点**。实测：agent 还占着文件时，
///   安装程序弹出「Setup was unable to automatically close all applications」
///   三选一对话框 —— 而这时应用**已经被关掉了**，用户面对的是「我的程序没了，
///   屏幕上有个看不懂的框」。这正是 roadmap 里那句「会下载但装不上」
/// - `/CLOSEAPPLICATIONS`：让 Restart Manager 去关占用文件的进程 ——
///   也就是我们自己。Inno 的 RM 会话只在这个开关（或 `[Setup]` 里的同名指令）
///   下才启动，不给就会卡在「文件正被占用」
/// - `/FORCECLOSEAPPLICATIONS`：RM 温和地关不掉的就强制关。同样是实测出来的：
///   RM 关掉了 GUI，却**没有**关掉控制台形态的 `cortex-local.exe`，
///   于是它一直占着自己那个文件。我们在这之前已经主动停过 agent，
///   这一条是给「停不掉」和「还有别的东西占着」兜底的
/// - `/RESTARTAPPLICATIONS`：装完把它关掉的那些再拉起来。这是「更新后自动
///   重启」的主路径
/// - `/NORESTART`：**不许重启 Windows**。安装程序认为需要重启整机时会直接
///   动手，而一个更新绝不该把用户正在做的别的事一起带走
///
/// # 为什么必须 detached
///
/// 安装程序是我们 fork 出来的子进程，而它接下来第一件事就是回头把我们关掉。
/// 不 detach 的话，父子的生命周期绑在一起，等于让它去杀自己的父亲然后指望
/// 自己活下来。
///
/// # 调用方的责任：先停掉本地 agent
///
/// 安装包会同时替换 `cortex-local.exe`。真机上量到的是：**RM 关掉了 GUI，
/// 却关不掉控制台形态的 agent**，于是它一直占着自己那个文件，安装因此停在
/// 那个模态框上（这正是 `/SUPPRESSMSGBOXES` 与 `/FORCECLOSEAPPLICATIONS`
/// 被加进来的原因）。
///
/// 那两个开关够兜底了，但**自己先停**仍然更好：force close 是从外面把一个
/// 正在写 outbox 的进程打断，而 [LocalAgentHandle.stop] 走的是它自己的
/// 退出路径。少一次不必要的强杀。
///
/// 顺带澄清一个当初的担心：`/RESTARTAPPLICATIONS` 并**没有**把 agent 当成
/// 独立程序重新拉起来（被强制关掉的进程 RM 不会重启），实测没有出现孤儿。
Future<void> launchInstaller(String setupPath) async {
  if (!File(setupPath).existsSync()) {
    throw UpdateException('安装程序不见了：$setupPath');
  }
  try {
    await Process.start(
      setupPath,
      installerArgs,
      mode: ProcessStartMode.detached,
    );
  } on ProcessException catch (e) {
    throw UpdateException('拉起安装程序失败：${e.message}');
  }
}

/// 退出应用，把替换与重启交给安装程序。
///
/// `exit(0)` 而不是关窗口：窗口关了进程未必走，而只要进程还在，
/// 被占用的 `cortex_app.exe` 就换不掉。
Never quitForUpdate() => exit(0);
