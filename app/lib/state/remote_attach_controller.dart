/// 「这台机器接不接受远程接入」—— 界面这一侧。
///
/// # 权威在哪
///
/// **运行时那个开关**（本机 agent 的 `GET/PUT /local/attach`）。界面显示的
/// 是它、拨动的也是它。设置里存的那一份只回答一个问题：**下次这个进程起来
/// 时是开还是关**（`local_agent_io.dart` 据此决定传不传 `--allow-remote-attach`）。
///
/// 两者短暂不一致是可能的 —— 比如有人从命令行手工起了一个带 flag 的 agent。
/// 那时以运行时那份为准：**显示真的那个**。反过来（显示存下来的那个）就是
/// 「界面写着关，云端接得进来」，而那正是这个开关存在的意义所在。
///
/// # 答不出来时**整个不画**
///
/// Web 端、纯 cortexd、以及比这条路由旧的本地 agent 都会 404。那时这个
/// provider 是 error 态，界面据此不画那一节 —— 而不是画一个永远关着的开关。
/// 与「电脑操作」那一节同一条纪律：做不到就别摆出来。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/cortex_api.dart';

import 'app_providers.dart';

/// 设置里那个键。**只是启动默认值**，见库文档。
const String kRemoteAttachSetting = 'remote_attach';

/// 这台机器现在开着没有。error = 这个后端答不出（见库文档）。
final remoteAttachProvider =
    AsyncNotifierProvider<RemoteAttachController, LocalAttach>(
      RemoteAttachController.new,
    );

class RemoteAttachController extends AsyncNotifier<LocalAttach> {
  @override
  Future<LocalAttach> build() async {
    // 换后端 / 换 agent 之后要重问：开关是**那台机器**的属性
    final api = ref.watch(cortexApiProvider);
    return api.localAttach();
  }

  /// 拨动它。回 `true` = 落定之后是开着的。
  ///
  /// 失败时**保持原状并把错误抛出去** —— 乐观地先把界面拨过去的话，
  /// 一次失败的开启会让用户以为自己开了。这个开关的两个状态风险不对等，
  /// 「以为开了其实没开」只是不好用，「以为关了其实开着」是安全问题，
  /// 所以两个方向都不乐观。
  Future<bool> set(bool enabled) async {
    final api = ref.read(cortexApiProvider);
    final settled = await api.setLocalAttach(enabled);
    state = AsyncData(settled);
    // 落盘的是**落定之后**那个值，不是用户点的那个：服务端才是权威
    await ref.read(settingsPatcherProvider)(
      kRemoteAttachSetting,
      settled.enabled ? 'true' : 'false',
    );
    return settled.enabled;
  }
}

/// 界面上那段话 —— **必须说破它交出了什么**（安全不变量 4）。
///
/// 接入面里 `POST /chat` 与 `POST /confirmations` 并存，也就是说接进来的
/// 一方能发起一轮、并**自己批准**那一轮弹出的工具确认。所以打开它等于同意
/// 远程侧可经模型在这台机器上执行命令与读写文件。
///
/// 写软成「允许远程查看」不会有任何报错，而用户对一个开关的全部理解就是
/// 它旁边这段话。`cortex-local` 那侧的 `--allow-remote-attach` 说明由一条
/// Rust 测试守着同一组词；这一段由 `remote_attach_wording_test.dart` 守着。
const String kRemoteAttachExplainer =
    '打开后，你在其他设备上的 Cortex 可以接进这台机器，继续那些绑在本机目录的会话。'
    '接进来的一方能发起对话，并自己批准工具确认 —— 也就是可以经模型在这台机器上'
    '执行命令、读写文件。默认关闭。';

/// 关掉之后会发生什么。用户按下「关」之前该知道的那一句。
const String kRemoteAttachOffNote = '关闭会立刻断开已经建立的连接；正在跑的那一轮仍会跑完。';
