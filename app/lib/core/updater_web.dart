/// Web 不给自己装版本 —— 它本来就是服务端发下来的，刷新即最新。
/// 见 `updater.dart` 里为什么这是诚实的答案而不是缺个功能。
///
/// 每个名字都与桌面侧对齐，调用方因此不需要任何 `kIsWeb`。
library;

import 'package:http/http.dart' as http;

import 'update_feed.dart';

const bool kUpdaterSupported = false;

/// 类型对得上而已。Web 上 [kUpdaterSupported] 是 false，走不到这里。
Future<String> downloadAndVerify(
  http.Client client,
  UpdateRelease release, {
  void Function(double fraction)? onProgress,
  Duration timeout = const Duration(minutes: 10),
}) async => throw const UpdateException('Web 构建不自我更新');

Future<void> launchInstaller(String setupPath) async =>
    throw const UpdateException('Web 构建不自我更新');

Never quitForUpdate() => throw const UpdateException('Web 构建不自我更新');

/// Web 没有进程环境变量可读。见 `updater_io.dart` 上那段为什么要有这个口子。
String? feedOverride() => null;
