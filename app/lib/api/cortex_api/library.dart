part of '../cortex_api.dart';

mixin CortexApiLibrary {
  /// `GET /folders` —— 文件夹清单（图片与资料**共用**，带项数与封面）。
  Future<Folders> folders();

  /// `GET /library` —— 资料库的一页。
  ///
  /// [folder] 只看某个文件夹（`"none"` = 只看未归档的）；
  /// [tab] 是 `all` / `images` / `files`。
  Future<LibraryPage> library({
    int limit,
    String? before,
    String? folder,
    String? tab,

    /// `size` = 按大小从大到小。**这一档不翻页** —— 它的用处是「库满了，
    /// 哪几份最占地方」，翻到第五页时早就不是最占地方的了。服务端因此
    /// 忽略 `before` 并回 `has_more: false`。
    String? sort,
  });

  /// `POST /library` —— 把一份**已登记的 blob** 收进资料库。
  ///
  /// 字节要先走 `/blobs`（与附件同一条路）。同一份内容重复收会回原来
  /// 那条而不是报错 —— 拖两次同一个文件不是错误。
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  });

  /// `PATCH /library/{id}` —— 改名 / 移动到文件夹。
  ///
  /// [folderId] 显式传 `null` 且 [moveFolder] 为真 = 移出文件夹。
  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  });

  /// `DELETE /library/{id}` —— 从资料库移除。**blob 不动。**
  Future<void> removeFromLibrary(String id);

  /// `POST /folders` —— 新建。回**整份列表**，客户端不必自己拼。
  Future<Folders> createFolder(String name);

  /// `PATCH /folders/{id}` —— 改名。
  Future<Folders> renameFolder(String id, String name);

  /// `DELETE /folders/{id}` —— 删文件夹。**里面的东西一件都不会没**，
  /// 它们回到「未归档」（服务端靠 `ON DELETE SET NULL` 保证）。
  Future<Folders> deleteFolder(String id);
}
