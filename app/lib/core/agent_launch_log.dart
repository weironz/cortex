import 'dart:convert';
import 'dart:io';

/// 本机 agent 的**启动诊断文件** —— 每次拉起、就绪、退出都记一行 JSON。
///
/// # 为什么内存里那份尾部不够
///
/// `LocalAgent._logTail` 已经收着子进程最后几行输出，但它**跟着进程一起死**：
///
/// - 崩溃循环时，每一轮的死因都被下一轮 `_logTail.clear()` 抹掉，
///   而用户看到的只有界面上一句「连不上」。2026-08-20 那次就是这样：
///   agent 每 6 秒换一个 PID，查了二十分钟才发现它在反复重启，
///   **而它每次死前都在 stderr 上说了原因**。
/// - 用户报障时也带不走。「一份文件就是全部现场」是 Codex 那条经验里
///   最实在的一半。
///
/// 所以要落盘，而且要**跨进程累积**：崩溃循环的形状（同一个死因重复 N 次）
/// 只有在多次启动的记录摆在一起时才看得出来。
///
/// # 为什么是 JSON Lines 而不是人读的日志
///
/// 它的第一读者不是人，是 `cortex doctor`（以及以后的报障上传）。
/// 一行一个自洽的对象，追加即写、截断不会毁掉其余行 —— 而人也照样读得懂。
///
/// # 边界
///
/// **写失败一律吞掉。** 诊断设施把被诊断的东西弄挂，是最糟的一类 bug：
/// 磁盘满、目录被杀毒软件锁住，都不该让 agent 起不来。
class AgentLaunchLog {
  AgentLaunchLog(this.file);

  /// 状态目录下的固定位置。与 agent 自己的 outbox 同一个目录树，
  /// 卸载时一并清掉（见 README 的残留说明）。
  factory AgentLaunchLog.inStateDir(String stateDir) {
    final sep = Platform.pathSeparator;
    return AgentLaunchLog(File('$stateDir${sep}logs${sep}agent-launch.jsonl'));
  }

  final File file;

  /// 超过这个大小就把前半截扔掉。
  ///
  /// 崩溃循环每 6 秒写一条、每条约 1 KB，一夜就是几十 MB —— 一个诊断文件
  /// 把用户磁盘吃掉，比没有这个文件更糟。上限取得足够大，能装下几百次启动，
  /// 而崩溃循环真正有用的信息本来就在**最后**那几十条里。
  static const int maxBytes = 256 * 1024;
  static const int keepBytes = 128 * 1024;

  /// 拉起了一个子进程。
  ///
  /// [llm] 记的是这一次让它怎么拿模型（离线是直连，否则经 cortexd 代理）——
  /// 「只有离线时才起不来」是一类真实故障，不记下来就看不出这个相关性。
  void spawn({
    required String exe,
    required int pid,
    required String remote,
    required String llm,
  }) =>
      _append('spawn', {'exe': exe, 'pid': pid, 'remote': remote, 'llm': llm});

  /// 子进程报出了它监听的地址。
  void ready({required String origin, required int ms}) =>
      _append('ready', {'origin': origin, 'ms': ms});

  /// 子进程退出了。
  ///
  /// [expected] 区分「我们让它退的」（登出、关应用、换地址）与崩溃 ——
  /// 不分的话，正常退出会在文件里堆成一片假故障，真的那条反而找不着。
  void exited({required int code, required bool expected, String? tail}) =>
      _append('exit', {
        'code': code,
        'expected': expected,
        if (tail != null && tail.isNotEmpty) 'tail': _lastLines(tail),
      });

  /// 压根没起来（找不到二进制、没报出地址、启动即退）。
  void failed({required String why, String? tail}) => _append('start-failed', {
    'why': why,
    if (tail != null && tail.isNotEmpty) 'tail': _lastLines(tail),
  });

  /// 客户端把用户送回了登录页 —— **带 reason code 落盘**。
  ///
  /// # 为什么它记在 agent 的启动日志里
  ///
  /// 「被踢回登录页」的几条路径在界面上是差不多的一句红字，而病因分属
  /// 三个家族（本地存储 / 服务端凭据 / 本机 agent 转发链）。2026-08-23
  /// 用户报「开着不动几十分钟就掉」时，`debugPrint` 跟着进程死了，日志里
  /// 什么都不剩 —— 这份文件是桌面端唯一跨进程活着的诊断通道，凑一条
  /// 时间线（agent 何时重启、何时被踢）恰好要它们在同一个文件里。
  void authGate({
    required String reason,
    required String detail,
    required bool wasReady,
  }) => _append('auth-gate', {
    'reason': reason,
    'detail': detail,
    // 从「正在用」掉下来才是事故；启动阶段的清场是常态。查日志的人
    // 靠这一位把两者分开，不然每次冷启动都像一次被踢
    'was_ready': wasReady,
  });

  /// 起来之后被我们**主动**停掉了（登出、换地址、provider 重建、退出）。
  ///
  /// 此前主动停**什么都不记**（stop() 先撤掉退出监听再 kill，exit 事件
  /// 无从写起），于是 2026-08-21 那个「agent 被 1 秒一次杀了 639+ 次」的
  /// 死循环在日志里只有一串 spawn/ready —— 看起来像它自己活不长，
  /// 而真相是**我们在杀它**。谁杀的必须留痕，否则下次还要考古半小时。
  void stopped() => _append('stopped', const {});

  /// 还在启动就被我们自己停掉了 —— **不是故障**。
  ///
  /// 每次冷启动都会发生一次：`localAgentOriginProvider` 依赖的东西
  /// （配置恢复、`requiresToken`、本机模型配置）在头几百毫秒里陆续落定，
  /// 每落定一次 Riverpod 就重建一次 provider，`onDispose` 顺手 `stop()`
  /// 掉上一轮那个还没报出地址的进程。
  ///
  /// 这一条必须与 [failed] 分开：不分的话，**每台机器每次开机都记一条
  /// 启动失败**，doctor 那边三条就报「崩溃循环」—— 一个自造的假故障，
  /// 而且专挑健康的机器报。2026-08-20 落地当天就撞上了。
  void superseded() => _append('superseded', const {});

  /// 尾部只留最后几行。
  ///
  /// 内存里那份最多 30 行，但里面可能有超长的单行（Rust 的 panic backtrace
  /// 就是），所以还要限长度：一条日志记录不该比它要诊断的问题还难读。
  static List<String> _lastLines(String tail, {int keep = 12}) {
    final lines = const LineSplitter().convert(tail);
    final from = lines.length > keep ? lines.length - keep : 0;
    return [
      for (final l in lines.sublist(from))
        l.length > 400 ? '${l.substring(0, 400)}…' : l,
    ];
  }

  void _append(String ev, Map<String, Object?> fields) {
    try {
      final dir = file.parent;
      if (!dir.existsSync()) dir.createSync(recursive: true);
      _rotateIfHuge();
      final line = jsonEncode({
        // ISO-8601 带时区：用户把文件发过来时，「什么时候」不能靠猜
        't': DateTime.now().toIso8601String(),
        'ev': ev,
        ...fields,
      });
      file.writeAsStringSync('$line\n', mode: FileMode.append, flush: true);
    } on Object {
      // 见类文档：诊断设施不许把被诊断的东西弄挂
    }
  }

  /// 超上限就砍掉前半截，**从行边界起**。
  ///
  /// 不从行边界切的话，第一行会是半条 JSON —— 读的人（和 doctor）要么解析
  /// 报错、要么得写一段容错逻辑，而那段逻辑只为这一个自找的问题存在。
  void _rotateIfHuge() {
    if (!file.existsSync() || file.lengthSync() <= maxBytes) return;
    final bytes = file.readAsBytesSync();
    var cut = bytes.length - keepBytes;
    if (cut < 0) cut = 0;
    // 0x0A = '\n'。往后找第一个换行，切在它之后
    while (cut < bytes.length && bytes[cut] != 0x0A) {
      cut++;
    }
    if (cut < bytes.length) cut++;
    file.writeAsBytesSync(bytes.sublist(cut), flush: true);
  }
}
