part of '../cortex_api.dart';

mixin CortexApiBlobs {
  /// `POST /blobs` — server-relayed upload, for content up to
  /// `kRelayUploadLimit`.
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/presign` — ask for a direct-to-object-store URL.
  ///
  /// Throws with `isUnsupported == true` on deployments backed by the local
  /// filesystem, which cannot sign URLs.
  Future<BlobPresign> presignBlob(String hash);

  /// `PUT` straight to the object store using a presigned URL. Not a cortexd
  /// endpoint — the whole point is that these bytes never touch the daemon.
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  });

  /// `POST /blobs/commit` — register a directly-uploaded object.
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  });

  /// `GET /blobs/{hash}` — the bytes, relayed by the daemon.
  ///
  /// Deliberately bytes rather than a URL: the mock source has no origin to
  /// hand out, and going through one method keeps the attachment widgets from
  /// having to know which backend they are on.
  Future<Uint8List> blobBytes(String hash);

  /// `GET /blobs/{hash}/url` —— 一条**会过期**的直链，给「复制链接」用。
  ///
  /// 与 [blobBytes] 是两件事：那条是**我自己要看**（中转，永远能用），
  /// 这条是**发给别人**（直链，对象存储签的，十五分钟）。
  ///
  /// 部署用本地文件系统当对象存储时签不出来，服务端回 501 ——
  /// 界面据此把按钮置灰**并说明**，而不是让它点下去弹一句红字。
  Future<BlobUrl> blobUrl(String hash);
}
