/// 账号资料的界面状态 —— 昵称、头像、以及删号那条带冷静期的路。
///
/// # 为什么是 AsyncNotifier 而不是把资料塞进 authState
///
/// 认证那份状态每次启动都要，而且它的错误态直接决定「进不进得去」。
/// 资料只有账号页要，而且它答不出来时**页面照样该开**（老服务端没有
/// `/auth/profile`）—— 混进去的话，一个 404 会被当成登录失效。
library;

import 'dart:typed_data';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/account.dart';
import 'app_providers.dart';

/// 当前账号的资料。error = 这个后端答不出（老版本 / 纯预共享 token 的部署）。
final profileProvider = AsyncNotifierProvider<ProfileController, Profile>(
  ProfileController.new,
  // **确定性的失败不重试。**
  //
  // 404 / 501 = 这个部署没有账号体系（老服务端、纯预共享 token）。riverpod
  // 默认会带退避一直重试，于是一个**永远不会成功**的请求被反复打 ——
  // 而界面因此停在 `AsyncLoading(error:)` 上，用户看到的是一个永远转的圈，
  // 而不是那句「这个部署没有账号体系」。写测试时撞到的。
  //
  // 其余（连不上、5xx）照旧退避重试：那些等一等真的可能好。
  retry: (count, error) {
    if (error is CortexApiException &&
        (error.isMissing || error.isUnsupported)) {
      return null;
    }
    final secs = [1, 3, 8, 20, 30];
    return Duration(seconds: secs[count.clamp(0, secs.length - 1)]);
  },
);

class ProfileController extends AsyncNotifier<Profile> {
  @override
  Future<Profile> build() async {
    // 换后端 / 换账号之后要重问 —— 资料是**那个账号**的属性
    final api = ref.watch(cortexApiProvider);
    return api.profile();
  }

  /// 改昵称。传 `null` = 清空。
  ///
  /// 失败时**保持原状并把错误抛出去**：乐观地先改界面的话，一次失败的保存
  /// 会让用户以为改成了，而下次打开又变回来 —— 那种「改了又没改」比一条
  /// 错误提示难受得多。
  Future<void> setNickname(String? nickname) async {
    final api = ref.read(cortexApiProvider);
    final next = await api.updateProfile(nickname: Patch(nickname));
    state = AsyncData(next);
  }

  Future<void> setAvatar(Uint8List bytes) async {
    final api = ref.read(cortexApiProvider);
    state = AsyncData(await api.putAvatar(bytes));
    // 头像变了，缓存作废 —— 不 invalidate 的话界面还画旧的那张
    ref.invalidate(avatarBytesProvider);
  }

  Future<void> clearAvatar() async {
    final api = ref.read(cortexApiProvider);
    state = AsyncData(await api.deleteAvatar());
    ref.invalidate(avatarBytesProvider);
  }

  /// 排期删号。回冷静期的终点。
  Future<DateTime?> requestDeletion(String password) async {
    final api = ref.read(cortexApiProvider);
    final next = await api.deleteAccount(password);
    state = AsyncData(next);
    return next.purgeAfter;
  }

  Future<void> cancelDeletion() async {
    final api = ref.read(cortexApiProvider);
    state = AsyncData(await api.restoreAccount());
  }
}

/// 头像的字节。`null` = 没有头像（或这个后端答不出）。
///
/// 单独一个 provider 而不是塞进 [Profile]：头像几十 KB，而资料每次打开
/// 账号页都要读 —— 合在一起的话每次都白传一遍那几十 KB。
final avatarBytesProvider = FutureProvider<Uint8List?>((ref) async {
  final profile = await ref.watch(profileProvider.future);
  if (!profile.hasAvatar) return null;
  final api = ref.watch(cortexApiProvider);
  try {
    return await api.avatarBytes(profile.userId);
  } on CortexApiException catch (e) {
    // 404 = 刚被清掉（或者 has_avatar 与实际不一致）。**不当成错误**：
    // 界面画默认头像就是了，为这个弹一条红字没有意义
    if (e.isMissing) return null;
    rethrow;
  }
});
