/// 一张图能做的事 —— **对话里那张与图库里那张共用这一份**。
///
/// # 为什么必须是同一份
///
/// 两处各写一遍的下场是「在对话里能复制链接，在图库里不能」，而用户根本
/// 分不清那是两个功能。真正的差别只有一个：**图库里那张手上就有 id**，
/// 对话里那张只有 blob 哈希。
///
/// 那个差别用类型表达（[ImageActions.galleryId] 可空），而不是靠两份代码；
/// 而且它是**可以补上的** —— 开着 [ImageActions.lookupByHash] 时，
/// 点下去那一刻会按哈希去问一次，于是对话里那张也能分享、也能移除。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/copy_image.dart';
import '../../../core/save_file.dart';
import '../../../state/app_providers.dart';
import '../../../state/blob_bytes.dart';
import '../../../state/image_controller.dart';
import 'folder_picker.dart';

/// 做完一件事之后要对用户说的那句话。
typedef ImageActionSaid = void Function(String message);

/// 一张图上能做的事。
class ImageActions {
  const ImageActions({
    required this.ref,
    required this.hash,
    required this.said,
    this.galleryId,
    this.prompt,
    this.shareUrl,
    this.onRemoved,
    this.lookupByHash = false,
  });

  final WidgetRef ref;

  /// blob 哈希 —— 取字节、复制、另存都靠它。
  final String hash;

  /// 说一句话（SnackBar / 查看器底下那一行）。
  final ImageActionSaid said;

  /// 图库里那一行的 id。`null` = 这张图只在对话里出现，
  /// 于是分享 / 移除 / 加相册这三件事**不成立**。
  final String? galleryId;

  /// 画它的那句话。`null` = 不知道（对话里的附件没有这个）。
  final String? prompt;

  /// 已经分享出去的链接。`null` = 还没分享过。
  final String? shareUrl;

  /// 从图库移除之后调一下（让调用方刷新）。
  final VoidCallback? onRemoved;

  /// 没有 [galleryId] 时，**按哈希去问一次**（`GET /images?hash=`）。
  ///
  /// 对话里那张图只带哈希，而 agent 画的每一张都在图库里。不去问的话，
  /// 同一张图在对话里右键出来的菜单比图库里少三项，用户会以为那是
  /// 两个不同的东西。
  ///
  /// 问是**点下去那一刻**才问，不是渲染时：一屏十几张图各发一个请求，
  /// 换来的只是几个菜单项的显隐。
  final bool lookupByHash;

  /// 分享 / 移除 / 加相册这三件事**可能**成立。
  ///
  /// 「可能」是诚实的：开着 [lookupByHash] 时，真答案要问过服务端才知道。
  /// 菜单据此显示，点下去之后若查无此行，会说清「这张图不在图库里」。
  bool get inGallery => galleryId != null || lookupByHash;

  /// 图库那一行的 id —— 没有就按哈希去问一次。
  Future<String?> _id() async {
    final known = galleryId;
    if (known != null) return known;
    if (!lookupByHash) return null;
    try {
      final page = await ref
          .read(cortexApiProvider)
          .gallery(limit: 1, hash: hash);
      // ⚠️ **回来的那一行必须自己核对哈希。**
      //
      // 老服务端不认得 `hash=` 这个参数，会**静默忽略它**、回最新那一页 ——
      // 直接取 `first.id` 的话，拿到的是另一张图的 id，而接下来发生的是
      // 「分享了别人的图」或者「移除了别人的图」，且两边都不报错。
      return page.items.where((i) => i.hash == hash).firstOrNull?.id;
    } on Object {
      return null;
    }
  }

  Future<Uint8List?> _bytes() async {
    try {
      return await ref.read(blobBytesProvider(hash).future);
    } on Object catch (e) {
      said('取不到这张图：$e');
      return null;
    }
  }

  /// 放进系统剪贴板。
  Future<void> copyImage() async {
    final b = await _bytes();
    if (b == null) return;
    final ok = await copyImageToClipboard(b);
    // 失败要**说出来**。静默当成功的表现是「粘出来还是上一次的东西」，
    // 而用户根本不会怀疑到这一步
    said(
      ok
          ? '图片已经放进剪贴板了。'
          : '这台机器上没能放进剪贴板 —— 可以用「另存为」，或者再试一次'
                '（剪贴板同一时刻只能被一个程序占着）。',
    );
  }

  /// 另存为 —— **弹系统对话框**，存完说清落在哪儿。
  Future<void> saveAs() async {
    final b = await _bytes();
    if (b == null) return;
    // 文件名带哈希前八位：同一句提示词画出来的几张，名字一样会互相覆盖
    final name = 'cortex-${hash.substring(0, 8)}.png';
    final path = await saveBytesAs(b, name, mimeType: 'image/png');
    // 取消不是失败，不该弹提示
    if (path == null) return;
    said('已存到 $path');
  }

  /// 复制一条**公开**链接。没分享过就先分享。
  Future<void> copyLink() async {
    final id = await _id();
    if (id == null) {
      said('这张图不在图库里，没法分享 —— 图库里那些才有链接。');
      return;
    }
    try {
      final url = shareUrl ?? await ref.read(cortexApiProvider).shareImage(id);
      await Clipboard.setData(ClipboardData(text: url));
      // ⚠️ **必须说清这是一条公开链接。** 它免登录、长期有效 ——
      // 用户以为自己只是「复制了个地址」，而那是把这张图放到了公网上
      // 星号在 `Text` 里会原样显示（`no_literal_markdown_test` 盯着这个）。
      // 靠措辞把重点说出来，不靠排版
      said(
        '链接已复制。这是一条公开链接：任何拿到它的人都能打开，不用登录；'
        '不想给了就点「停止分享」。${reachOnly(url)}',
      );
      onRemoved?.call(); // 借这条回调刷新（分享状态变了）
    } on CortexApiException catch (e) {
      said(e.isUnsupported ? '这个部署不支持分享。' : e.message);
    } on Object catch (e) {
      said('$e');
    }
  }

  /// 撤销分享。那条链接**当场**失效。
  Future<void> unshare() async {
    final id = await _id();
    if (id == null) return;
    try {
      await ref.read(cortexApiProvider).unshareImage(id);
      said('已停止分享 —— 那条链接现在打不开了。');
      onRemoved?.call();
    } on Object catch (e) {
      said('$e');
    }
  }

  /// 移进一个文件夹（`null` = 移出来）。
  ///
  /// # 为什么单张也要有这个入口
  ///
  /// 归档此前**只有批量那条路**：先进多选、勾中、再点顶上的按钮。
  /// 而「看到一张想归类」是单张场景 —— 为了归一张图先进多选模式，
  /// 是让人绕一圈去够一个本该就在手边的动作。2026-08-27 用户实测报的。
  Future<void> moveToFolder(String? folderId, String folderName) async {
    final id = await _id();
    if (id == null) {
      said('这张图不在图库里 —— 没有可归档的那一行。');
      return;
    }
    try {
      await ref.read(cortexApiProvider).moveImage(id, folderId);
      // 文件夹那一行上的数字要跟上；当前这一页也可能因此少一张
      ref.invalidate(foldersProvider);
      said(folderId == null ? '已移出文件夹。' : '已移进「$folderName」。');
      onRemoved?.call();
    } on Object catch (e) {
      said('$e');
    }
  }

  /// 从图库移除。**blob 不动**，对话里那张照常显示。
  Future<void> removeFromGallery() async {
    final id = await _id();
    if (id == null) {
      said('这张图不在图库里 —— 没有可移除的那一行。');
      return;
    }
    try {
      await ref.read(cortexApiProvider).removeImage(id);
      said('已从图库移除（对话里那张还在）。');
      onRemoved?.call();
    } on Object catch (e) {
      said('$e');
    }
  }
}

/// 菜单里那一项的落点：挑一个文件夹，然后把这张图移过去。
Future<void> _pickFolder(BuildContext context, ImageActions actions) async {
  final folder = await pickFolder(context, actions.ref, said: actions.said);
  if (folder == null) return;
  await actions.moveToFolder(folder.id, folder.name);
}

/// 这条链接**其实发不出去**时补的那一句。没问题就回空串。
///
/// # 为什么非说不可
///
/// 上面那句「任何拿到它的人都能打开」在**回环 / 内网**地址上是假的：
/// `http://127.0.0.1:5173/s/...` 只有这台机器打得开。用户把它发给同事，
/// 对方看到的是连接被拒 —— 而他会以为是分享坏了，不会想到是地址的问题。
///
/// 链接怎么拼是服务端的事（`public_base`：`CORTEX_PUBLIC_URL` →
/// `X-Forwarded-Host` → `Host`），生产上它就是域名。这里不改地址，
/// 只是把「这条地址能走多远」如实说出来。
String reachOnly(String url) {
  final host = Uri.tryParse(url)?.host ?? '';
  if (host.isEmpty) return '';
  final loopback =
      host == 'localhost' || host == '::1' || host.startsWith('127.');
  // 10/8、192.168/16、172.16–31/12 —— 家用与办公网段
  final lan =
      host.startsWith('10.') ||
      host.startsWith('192.168.') ||
      RegExp(r'^172\.(1[6-9]|2\d|3[01])\.').hasMatch(host);
  if (loopback) {
    return ' 不过这条链接指向 $host —— 只有这台机器上打得开，'
        '发给别人是打不开的（这个部署还没有对外的域名）。';
  }
  if (lan) return ' 不过这条链接指向内网地址 $host —— 只有同一个网络里的人打得开。';
  return '';
}

/// 右键菜单。桌面与 Web 走同一条路。
///
/// 为什么不是长按：桌面上长按不是一个手势，而这个应用的主场是桌面。
/// 触屏那侧仍然能用 —— 点开大图，那边所有动作都摆在底部一排。
Future<void> showImageContextMenu(
  BuildContext context,
  Offset globalPosition,
  ImageActions actions,
) async {
  final overlay = Overlay.of(context).context.findRenderObject()! as RenderBox;
  await showMenu<void>(
    context: context,
    position: RelativeRect.fromRect(
      globalPosition & const Size(1, 1),
      Offset.zero & overlay.size,
    ),
    items: [
      PopupMenuItem(onTap: actions.copyImage, child: const Text('复制图片')),
      PopupMenuItem(onTap: actions.saveAs, child: const Text('另存为…')),
      if (actions.inGallery)
        PopupMenuItem(
          onTap: actions.copyLink,
          // 已经分享过的**标出来**：否则用户每次都在问「我是不是已经
          // 把这张放到公网上了」，而这个菜单答得上来
          child: Text(actions.shareUrl == null ? '复制图片链接' : '复制图片链接（已分享）'),
        ),
      if (actions.shareUrl != null)
        PopupMenuItem(onTap: actions.unshare, child: const Text('停止分享')),
      if (actions.inGallery) ...[
        const PopupMenuDivider(),
        // 归档。**单张也给** —— 此前只有多选那条路，为了归一张图
        // 得先进多选模式，见 `ImageActions.moveToFolder`
        PopupMenuItem(
          onTap: () => _pickFolder(context, actions),
          child: const Text('移动到文件夹…'),
        ),
        PopupMenuItem(
          onTap: actions.removeFromGallery,
          // **叫「从图库移除」，不叫「删除图片」** —— blob 不动，
          // 对话里那张照常显示。叫删除，用户会以为历史里那张也没了
          child: const Text('从图库移除'),
        ),
      ],
    ],
  );
}
