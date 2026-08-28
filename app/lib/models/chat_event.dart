import 'attachment.dart';
import 'json.dart';
import 'pending_confirmation.dart';

/// One decoded frame of the `POST /chat` SSE stream.
///
/// Wire contract (each SSE `data:` payload is one JSON object):
/// * `{"type":"tool","name":"read_file","summary":"...","path":"src/main.rs"}`
/// * `{"type":"delta","text":"..."}`
/// * `{"type":"confirm","token":"...","tool":"shell","risk":"execute",
///    "preview":"command: rm -rf …","timeout_secs":180}`
/// * `{"type":"queued","ahead":1}`
/// * `{"type":"done","episode_id":"01J..."}`
/// * `{"type":"error","message":"..."}`
///
/// Observed order: `tool` → N×`delta` → `done`. Nothing in the client depends
/// on that order, though — `tool` may interleave with `delta` once the agent
/// loop does multi-step tool use.
///
/// Unknown `type` values decode to [ChatUnknownEvent] rather than throwing, so
/// a server that grows a new event type does not break older clients.
sealed class ChatEvent {
  const ChatEvent();

  factory ChatEvent.fromJson(Map<String, dynamic> json) {
    return switch (asString(json['type'])) {
      'queued' => ChatQueuedEvent(asIntOrNull(json['ahead']) ?? 1),
      'delta' => ChatDeltaEvent(asString(json['text'])),
      'tool' => ChatToolEvent(
        name: asString(json['name'], 'tool'),
        summary: asStringOrNull(json['summary']),
        path: asStringOrNull(json['path']),
        diff: asStringOrNull(json['diff']),
        phase: ToolPhase.fromWire(json['phase']),
      ),
      'confirm' => ChatConfirmEvent(
        PendingConfirmation(
          token: asString(json['token']),
          tool: asString(json['tool'], 'tool'),
          risk: asString(json['risk'], 'execute'),
          scope: asStringOrNull(json['scope']),
          diff: asStringOrNull(json['diff']),
          preview: asString(json['preview']),
          // `timeout_secs` here, `expires_in_secs` on the recovery endpoint:
          // the same quantity named for the two different questions it answers
          // ("how long do they get" vs "how long is left"). Both are durations
          // from *now*, which is why neither path has to trust a clock.
          deadline: DateTime.now().add(
            Duration(
              seconds:
                  asIntOrNull(json['timeout_secs']) ??
                  PendingConfirmation.kDefaultTimeoutSecs,
            ),
          ),
        ),
      ),
      'done' => ChatDoneEvent(
        asStringOrNull(json['episode_id']),
        models: asStringList(json['models']),
        attachments: asObjectList(
          json['attachments'],
        ).map(Attachment.fromJson).toList(growable: false),
      ),
      'error' => ChatErrorEvent(
        asStringOrNull(json['message']) ?? '服务端返回了一个未描述的错误',
      ),
      final other => ChatUnknownEvent(other, json),
    };
  }
}

/// 这一轮排在队里，前面还有 [ahead] 轮没跑完。
///
/// **不是终态。** 同一条流接着会送这一轮自己的 delta / tool，最后一条 `done`。
///
/// 为什么服务端要发它：一个会话一次只跑一轮，排队期间这条流上除了 keepalive
/// 什么都没有 —— 不说一声的话界面就是一个转了几分钟的圈，与卡死一模一样。
final class ChatQueuedEvent extends ChatEvent {
  const ChatQueuedEvent(this.ahead);

  /// 前面还有几轮。服务端保证 `>= 1`；缺字段时按 1 处理（说「在排队」总比
  /// 说「排在第 0 位」好）。
  final int ahead;
}

/// Incremental assistant text. Append, never replace.
final class ChatDeltaEvent extends ChatEvent {
  const ChatDeltaEvent(this.text);
  final String text;
}

/// The agent invoked a tool. Rendered as a collapsible one-liner in the
/// conversation, not as message content.
/// 一条 [ChatToolEvent] 说的是「要调了」还是「调完了」。
///
/// # 缺省是 [result] 而不是 [call]
///
/// 老服务端不下发这个字段。按 `result` 走 = **不画进行中的占位**；
/// 按 `call` 走 = 画一个**永远不会消失**的占位（那台服务端也不会发
/// 第二条来撤它）。少一个动画，好过界面上永久卡着一块「正在生成」。
enum ToolPhase {
  call,
  result;

  static ToolPhase fromWire(Object? v) =>
      v == 'call' ? ToolPhase.call : ToolPhase.result;
}

final class ChatToolEvent extends ChatEvent {
  const ChatToolEvent({
    required this.name,
    this.summary,
    this.path,
    this.diff,
    this.phase = ToolPhase.result,
  });
  final String name;
  final String? summary;

  /// 要调了，还是调完了。
  ///
  /// 单独一个字段而不是从 [summary] 里正则抠 —— 与 [path] / [diff] 同一条
  /// 理由：summary 是给人看的一句话，措辞随时会改，而这里猜错的表现是
  /// 「正在生成」那块占位要么永远不出现、要么永远不消失。
  final ToolPhase phase;

  /// The file this call touched, or null for a tool that touches none.
  /// Optional in the contract precisely so that a tool touching no file can
  /// omit it rather than send an empty string the UI would render as a path.
  final String? path;

  /// 这次写入改了什么。只随**结果**那条事件到达 —— 调用那一刻还没执行，
  /// 也就还没有改动可言。
  final String? diff;
}

/// The turn is **suspended** until this is answered.
///
/// Not a terminal frame and not an error: the stream stays open, deltas simply
/// stop arriving until a receipt reaches the daemon (or its timeout fires, and
/// silence counts as a refusal). The UI must therefore keep the streaming
/// bubble alive while showing the prompt — treating this like an error would
/// discard text the model already produced.
final class ChatConfirmEvent extends ChatEvent {
  const ChatConfirmEvent(this.request);
  final PendingConfirmation request;
}

/// Terminal frame. [episodeId] is the archived assistant episode.
final class ChatDoneEvent extends ChatEvent {
  const ChatDoneEvent(
    this.episodeId, {
    this.models = const [],
    this.attachments = const [],
  });
  final String? episodeId;

  /// 这一轮**先后**是谁写的，按发生顺序。
  ///
  /// 带在终帧上而不是等下次拉会话：那样当前这一轮要刷新才看得到标签。
  /// 空 = 不知道（供应商没报、或者老服务端不发这个字段）——
  /// 界面据此什么都不画，不猜一个填上去。
  final List<String> models;

  /// 这一轮**工具产出**的附件（当下只有 `generate_image` 画的图）。
  ///
  /// # ⚠️ 没有它，画出来的图要重新拉一次会话才看得见
  ///
  /// 流式这条路只见过 delta 与工具事件，从来不知道这一轮往 episode 上挂了
  /// 什么 blob。于是收尾时拼出来的那条消息没有附件 —— 用户看到模型说
  /// 「画好啦」，而屏幕上一张图都没有。2026-08-23 实测到的就是这个。
  ///
  /// 空 = 这一轮没产出附件，**或者**服务端是个不发这个字段的老版本
  /// （那时维持从前的行为：刷新之后才见到，不会更坏）。
  final List<Attachment> attachments;
}

/// Terminal frame carrying a server-side failure.
final class ChatErrorEvent extends ChatEvent {
  const ChatErrorEvent(this.message);
  final String message;
}

/// 服务端还活着 —— SSE 的 keep-alive 注释帧（`: ping`，每 15 秒一次）。
///
/// # 为什么它要一路走到 controller
///
/// 线上没有这个 `type`，[ChatEvent.fromJson] 永远不会造出它；它由
/// `HttpCortexApi._events` 就地合成。这是有意的：**判活的证据只有它**。
///
/// 数据帧之间可以合法地静默很久 —— agent 在跑一条十分钟的命令、或者这一轮
/// 排在别人后面（[ChatQueuedEvent]）—— 所以「多久没有 delta」判不了死活，
/// 拿它当判据只会误杀正在干活的那一轮。心跳不一样：它是「服务端确实还在
/// 往这条连接上写字节」的直接证据，停了就是真停了。
///
/// 不进转录、不改任何状态，唯一的作用是把 `ChatController` 那条空转看门狗
/// 的表拨回去。
final class ChatHeartbeatEvent extends ChatEvent {
  const ChatHeartbeatEvent();
}

/// Forward-compatibility escape hatch.
final class ChatUnknownEvent extends ChatEvent {
  const ChatUnknownEvent(this.type, this.raw);
  final String type;
  final Map<String, dynamic> raw;
}
