/// 按 blob 哈希取字节，**取过一次就不再取**。
///
/// # 为什么可以无脑缓存
///
/// blob 是内容寻址的：同一个哈希永远是同一份字节。于是这里没有
/// 「什么时候失效」这个问题 —— 那正是内容寻址买来的东西。
///
/// # 为什么是 provider 而不是各组件自己一个 static Map
///
/// 附件缩略图与画廊是两处，各存一份的结果是同一张图在两个地方各下载一遍
///（而画廊里那些图恰恰也会出现在对话里）。放进 provider 还顺带让测试能
/// 覆盖它：`cortexApiProvider` 一换，这一层跟着换。
library;

import 'dart:typed_data';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app_providers.dart';

/// 一个哈希的字节。
///
/// **不是 autoDispose**：画廊里滚出屏幕再滚回来是常态，
/// 自动释放的话每次回滚都要重新下载一遍。
final blobBytesProvider = FutureProvider.family<Uint8List, String>(
  (ref, hash) => ref.watch(cortexApiProvider).blobBytes(hash),
  // 取图失败多半是网络抖了一下，值得重试；而「这个 blob 不存在」
  // 重试永远不会成功 —— 与模型目录那条同一个判据
  retry: (count, error) {
    final secs = [1, 3, 8];
    return count >= secs.length ? null : Duration(seconds: secs[count]);
  },
);
