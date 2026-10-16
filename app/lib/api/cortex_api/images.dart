part of '../cortex_api.dart';

mixin CortexApiImages {
  /// `POST /llm/image` —— 画几张图，回它们的 blob 哈希。
  ///
  /// # 为什么只回哈希
  ///
  /// 图在服务端就抓下来入库了（供应商给的链接只活 24 小时）。取图与附件
  /// 走同一条路（[blobBytes]），所以这里不需要第二种「图片从哪来」。
  ///
  /// 提示词、型号、时间这些由画廊（[gallery]）回答 —— 画完刷一次即可，
  /// 一份数据一个来源。
  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  });

  /// `GET /images` —— 画廊，按时间倒序翻页。
  ///
  /// [folder] 只看某个文件夹里的（`"none"` = 只看未归档的）；
  /// [hash] 只看某份字节对应的那一行。
  ///
  /// [hash] 是给**对话里那张图**用的：附件只带哈希，而分享 / 移除要的是
  /// 画廊那一行的 id。没有它，同一张图在对话里右键出来的菜单会比图库里
  /// 少几项 —— 而用户根本分不清那是两个东西。
  Future<Gallery> gallery({
    int limit,
    String? before,
    String? folder,
    String? hash,
  });

  /// `POST /images/{id}/share` —— 拿一条**公开**链接（免登录、长期有效）。
  ///
  /// 重复调用回同一条。界面上必须说清它是公开的 —— 用户以为自己只是
  /// 「复制了个地址」，而那是把这张图放到了公网上。
  Future<String> shareImage(String id);

  /// `DELETE /images/{id}/share` —— 撤销。那条链接当场 404。
  Future<void> unshareImage(String id);

  /// `DELETE /images/{id}` —— 从图库移除。
  ///
  /// **blob 不动**：对话里那张图照常显示。所以界面上这个动作叫
  /// 「从图库移除」，不叫「删除图片」。
  Future<void> removeImage(String id);

  /// `PATCH /images/{id}` —— 把一张图移进 / 移出文件夹。
  ///
  /// 传 `null` = 移出来。**归档是排他的**，所以「移进 A」自带「离开 B」——
  /// 不需要客户端拆成两次请求（拆了的话中间断网就是一张谁也不属于的图）。
  Future<void> moveImage(String id, String? folderId);
}
