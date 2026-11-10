import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart' show visibleForTesting;

import 'agent_launch_log.dart';
import 'agent_credentials.dart';
import 'computer_use_runtime_io.dart';

/// Desktop can run an agent. See `local_agent.dart` for the seam's rationale.
const bool kLocalAgentSupported = true;

/// Environment override for development.
///
/// `flutter run` puts the app's exe in a build directory that has no
/// `cortex-local` next to it, so discovery would find nothing and the whole
/// local path would silently never be exercised until packaging.
const String kAgentPathEnvVar = 'CORTEX_LOCAL_BIN';

/// Safe first-run app candidates; the per-turn Computer Use switch stays off.
List<String> defaultLocalComputerUseAllowlist() =>
    defaultComputerUseAllowlist();

/// Locates the bundled agent, or null when this build has none beside it.
///
/// Returning null is a **normal** outcome, not an error: a developer running
/// `flutter run` without the override has no agent, and the app must still
/// work by talking to the remote directly.
LocalAgent? discoverLocalAgent() {
  final override = Platform.environment[kAgentPathEnvVar]?.trim();
  final candidate = (override != null && override.isNotEmpty)
      ? override
      : _besideTheApp();
  if (candidate == null || !File(candidate).existsSync()) return null;
  final state = stateDir();
  if (state == null) return null;
  return LocalAgent(executable: candidate, stateDir: state);
}

/// The installer drops `cortex-local` next to `cortex_app.exe`, so the app's
/// own location is the answer — not a PATH lookup. A PATH hit could be some
/// other, differently-versioned copy, and the protocol between the two is
/// versioned only by "they shipped together".
String? _besideTheApp() {
  final dir = File(Platform.resolvedExecutable).parent.path;
  final name = Platform.isWindows ? 'cortex-local.exe' : 'cortex-local';
  return '$dir${Platform.pathSeparator}$name';
}

/// Must match `cortex_local::config::state_dir` — the agent writes its outbox
/// and workspace bindings there, and the address handshake file goes alongside.
///
/// 公开（去掉下划线）是给 `single_instance_io.dart` 用的：单实例锁必须
/// 落在**同一个**状态目录里，锁的正是「谁拥有这个目录」。再抄一份解析
/// 逻辑的话，两处漂移的后果是锁和数据各守一个目录 —— 等于没锁。
String? stateDir() {
  if (Platform.isWindows) {
    final base = Platform.environment['LOCALAPPDATA'];
    return (base == null || base.isEmpty)
        ? null
        : '$base${Platform.pathSeparator}cortex';
  }
  final xdg = Platform.environment['XDG_DATA_HOME'];
  if (xdg != null && xdg.isNotEmpty) return '$xdg/cortex';
  final home = Platform.environment['HOME'];
  return (home == null || home.isEmpty) ? null : '$home/.local/share/cortex';
}

/// Supervises the bundled `cortex-local` process.
///
/// ## Why the desktop app spawns an agent at all
///
/// Tools have to run **where your files are**. Before this, the agent loop ran
/// inside `cortexd` — on a server — so it could not see the code on your
/// machine. `cortex-local` is that loop, moved here; memory still lives in the
/// one remote authority.
///
/// ## The app keeps talking to one base URL, it just changes which one
///
/// `cortex-local` speaks the *same* protocol as `cortexd` and reverse-proxies
/// everything it does not handle. So the whole client stays pointed at a single
/// origin — it becomes `http://127.0.0.1:<port>` instead of the remote. Nothing
/// else in the app needs to know an agent exists.
///
/// ## Port
///
/// Bound as `127.0.0.1:0` — the kernel picks a free one and the child writes
/// the real address back. Picking a fixed port here would collide with a second
/// instance, or with unrelated software, and the failure mode of guessing is
/// the bad one: connecting to *something else* that happens to be listening.
///
/// ## Lifetime
///
/// Killed on [stop]. The child *also* watches this process (`--parent-pid`) and
/// exits on its own if the GUI dies without cleanup — a crash, a Task Manager
/// kill, a force-close. An orphan here would hold a port, hold the user's
/// token, and still be able to execute commands, all while the user believes
/// they have quit.
class LocalAgent {
  LocalAgent({required this.executable, required this.stateDir})
    : _startProcess = Process.start,
      _computerUseFactory = ComputerUseRuntime.new,
      _startupClock = Stopwatch.new;

  /// 测试可替换进程句柄、准备器与预算时钟，不拉起真实 agent、GUI 或 Cua。
  @visibleForTesting
  LocalAgent.forTesting({
    required this.executable,
    required this.stateDir,
    required this._startProcess,
    required this._computerUseFactory,
    Stopwatch Function()? startupClock,
  }) : _startupClock = startupClock ?? Stopwatch.new;

  /// Absolute path to `cortex-local` (`.exe` on Windows).
  final String executable;

  /// Where the address handshake file goes. Same directory the agent already
  /// uses for its outbox and workspace bindings.
  final String stateDir;
  final ComputerUseProcessStarter _startProcess;
  final ComputerUseRuntime Function({required String stateDir})
  _computerUseFactory;
  final Stopwatch Function() _startupClock;

  Process? _process;
  String? _origin;
  StreamSubscription<void>? _exitWatch;
  final List<StreamSubscription<void>> _pipes = [];
  ComputerUseRuntime? _computerUse;
  Future<void>? _computerUseStopping;
  Future<void>? _agentStopping;
  Future<Process>? _agentSpawn;
  bool _preparing = false;
  int _generation = 0;
  String? _lastCleanupError;

  String? get lastCleanupError => _lastCleanupError;

  /// Set by [stop] so the exit that follows is not reported as a crash.
  bool _stopping = false;

  /// Last lines the child printed, oldest first.
  ///
  /// Kept rather than discarded because this is the **only** place the reason
  /// for a death survives. `cortex-local` refuses to start on a protocol
  /// mismatch, and that refusal is one line on stderr — throw the pipe away
  /// and the user gets "the agent is not running" with no way to learn why.
  final List<String> _logTail = [];
  static const _tailLines = 30;

  /// 落盘那一份。[_logTail] 跟着进程死，这一份跨进程活着 —— 崩溃循环的
  /// 形状（同一个死因重复 N 次）只有在多次启动的记录摆在一起时才看得出来。
  late final AgentLaunchLog _launchLog = AgentLaunchLog.inStateDir(stateDir);

  /// 启动诊断文件的路径，给报错文案与 `cortex doctor` 用。
  String get launchLogPath => _launchLog.file.path;

  /// `http://127.0.0.1:<port>` once running, else null.
  String? get origin => _origin;

  bool get isRunning => _process != null;

  /// Starts the agent and waits until it reports the address it bound.
  ///
  /// Throws [LocalAgentException] when the binary is missing, refuses to start,
  /// or never reports an address. **Callers should fall back to talking to the
  /// remote directly** rather than blocking sign-in: an agent that will not
  /// start costs you local tools, not the whole app.
  /// [onExit] fires only for an exit **we did not ask for** — a crash, an OOM
  /// kill, a refusal to start. A [stop] never triggers it, so the caller can
  /// treat every call as "this needs handling" without tracking intent itself.
  ///
  /// It receives the exit code and the tail of what the child printed. The tail
  /// is the whole point: without it, "the agent died" is unactionable.
  Future<String> start({
    required String remote,
    required String token,
    String? userToken,
    String? llmRoute,
    bool allowRemoteAttach = false,
    List<String> computerUseAllowlist = const [],
    List<String> packagedComputerAllowlist = const [],
    List<String> browserSiteAllowlist = const [],
    Map<String, String>? extraEnv,
    Duration timeout = const Duration(seconds: 20),
    void Function(int code, String logTail)? onExit,
  }) async {
    if (_agentStopping != null || _agentSpawn != null || _preparing) {
      throw const LocalAgentException('本地 agent 正在准备或停止，不能重复启动');
    }
    if (_process != null) {
      final origin = _origin;
      if (!_stopping && origin != null) return origin;
      throw const LocalAgentException('上一轮本地 agent 尚未确认退出，保留其配置');
    }
    _stopping = false;
    final generation = ++_generation;
    _lastCleanupError = null;
    _logTail.clear();
    final startedAt = DateTime.now();
    final startupBudget = _startupClock()..start();

    final exe = File(executable);
    if (!exe.existsSync()) {
      _launchLog.failed(why: '找不到本地 agent：$executable');
      throw LocalAgentException('找不到本地 agent：$executable');
    }

    // A **unique** handshake file per start, rather than a fixed name plus a
    // staleness check.
    //
    // The fixed-name version compared the file's mtime against the moment we
    // spawned. That is broken on Windows: `FileStat.modified` truncates to
    // whole seconds, while `DateTime.now()` carries microseconds — so a file
    // written 200 ms *after* the spawn reports an mtime up to a second
    // *before* it, and the address is rejected forever. Measured, not guessed:
    // `startedAt=…53.768548  mtime=…53.000`.
    //
    // Making the name unique removes the question instead of tuning the
    // heuristic: a leftover file from a previous run cannot collide with this
    // run's, so there is nothing to date-check.
    final stamp = DateTime.now().microsecondsSinceEpoch.toRadixString(36);
    final addrFile = File(
      '$stateDir${Platform.pathSeparator}agent-$pid-$stamp.addr',
    );
    _sweepStaleHandshakes(Directory(stateDir));

    // 两把凭据只走环境，不进 argv；外部模型配置不能覆盖认证项。
    final env = AgentLaunchCredentials(
      inboundToken: token,
      userToken: userToken,
    ).environment(extraEnv);

    Duration remainingStartupBudget() {
      final remaining = timeout - startupBudget.elapsed;
      return remaining.isNegative ? Duration.zero : remaining;
    }

    final previousStopBudget = remainingStartupBudget();
    if (previousStopBudget <= Duration.zero) {
      const headline = '本地 agent 启动准备超过总预算';
      _launchLog.failed(why: headline);
      throw LocalAgentException(
        '$headline（${timeout.inSeconds} 秒）\n完整启动记录：$launchLogPath',
      );
    }
    final hadPreviousComputerUse =
        _computerUse != null || _computerUseStopping != null;
    var previousSidecarStopTimedOut = false;
    await _stopComputerUse().timeout(
      previousStopBudget,
      onTimeout: () {
        previousSidecarStopTimedOut = true;
        _lastCleanupError = '上一轮 Computer Use 清理超过启动总预算';
        _launchLog.cleanupFailed(
          component: 'computer-use',
          error: _lastCleanupError!,
        );
      },
    );
    if (previousSidecarStopTimedOut) {
      const headline = '上一轮 Computer Use 清理没有在启动总预算内完成';
      _launchLog.failed(why: headline);
      throw LocalAgentException(
        '$headline（${timeout.inSeconds} 秒）\n完整启动记录：$launchLogPath',
      );
    }
    if (hadPreviousComputerUse && _lastCleanupError != null) {
      const headline = '上一轮 Computer Use 没有完成可靠清理';
      _launchLog.failed(why: '$headline：$_lastCleanupError');
      throw LocalAgentException(
        '$headline：$_lastCleanupError\n完整启动记录：$launchLogPath',
      );
    }
    final computerUse = _computerUseFactory(stateDir: stateDir);
    _computerUse = computerUse;
    final computerUseStartedAt = DateTime.now();
    final sidecarBudget = remainingStartupBudget();
    if (sidecarBudget <= Duration.zero) {
      const headline = 'Computer Use 启动前总预算已经耗尽';
      _launchLog.failed(why: headline);
      throw LocalAgentException(
        '$headline（${timeout.inSeconds} 秒）\n完整启动记录：$launchLogPath',
      );
    }
    var preparationTimedOut = false;
    _preparing = true;
    late final Map<String, String> computerUseEnv;
    try {
      computerUseEnv = await computerUse
          .prepareForAgent(
            allowlist: computerUseAllowlist,
            packagedAllowlist: packagedComputerAllowlist,
            timeout: sidecarBudget,
          )
          .timeout(
            sidecarBudget,
            onTimeout: () async {
              preparationTimedOut = true;
              final cleanupBudget = remainingStartupBudget();
              if (cleanupBudget > Duration.zero) {
                await computerUse.stop().timeout(
                  cleanupBudget,
                  onTimeout: () {},
                );
              } else {
                unawaited(computerUse.stop());
              }
              return const <String, String>{};
            },
          );
    } finally {
      _preparing = false;
    }
    if (generation != _generation || _stopping) {
      await _stopComputerUse();
      _launchLog.superseded();
      throw const LocalAgentException('本地 agent 启动准备已被停止');
    }
    final computerUseError = preparationTimedOut
        ? 'Computer Use 配置准备超过总启动预算'
        : computerUse.lastError;
    final computerUseRequested =
        computerUseAllowlist.isNotEmpty || packagedComputerAllowlist.isNotEmpty;
    _launchLog.computerUse(
      outcome: computerUseEnv.isNotEmpty
          ? 'prepared'
          : computerUseError == null && !computerUseRequested
          ? 'disabled'
          : 'failed',
      ms: DateTime.now().difference(computerUseStartedAt).inMilliseconds,
      error: computerUseError,
      runtimeOwner: computerUseEnv.isNotEmpty ? 'agent' : null,
    );
    if (computerUseEnv.isNotEmpty) {
      env.addAll(computerUseEnv);
    }
    if (browserSiteAllowlist.isNotEmpty) {
      env['CORTEX_BROWSER_ENABLE'] = '1';
      env['CORTEX_BROWSER_SITE_ALLOWLIST'] = browserSiteAllowlist.join(',');
    }
    if (llmRoute != null) {
      // 离线模式要它本地直连模型 —— 代理那条路要经 cortexd，而它不在。
      // 不给这一项就不干预，让 agent 按自己的环境变量决定（默认 proxy）
      env['CORTEX_LOCAL_LLM'] = llmRoute;
    }

    if (remainingStartupBudget() <= Duration.zero) {
      await _stopComputerUse();
      const headline = '本地 agent 启动准备超过总预算';
      _launchLog.failed(why: headline);
      throw LocalAgentException(
        '$headline（${timeout.inSeconds} 秒）\n完整启动记录：$launchLogPath',
      );
    }

    final Process proc;
    Future<Process>? spawning;
    try {
      spawning = _startProcess(executable, [
        '--bind',
        // 端口交给内核分配，避免连到旧实例或其他进程。
        '127.0.0.1:0',
        '--remote',
        remote,
        '--parent-pid',
        '$pid',
        '--addr-file',
        addrFile.path,
        // 上次拨到「开」的话，这次起来就是开着的。
        //
        // ⚠️ **只是启动默认值。** 权威是运行时那个开关
        // （`PUT /local/attach`）—— 界面显示的、拨动的都是它。
        // 这里读的那份设置只回答「这个进程起来时是开还是关」，
        // 两者短暂不一致时**以运行时那份为准**（它才是真的）。
        if (allowRemoteAttach) '--allow-remote-attach',
      ], environment: env);
      _agentSpawn = spawning;
      proc = await spawning.timeout(remainingStartupBudget());
      _agentSpawn = null;
    } on TimeoutException {
      _stopping = true;
      _generation++;
      // Process.start 不能取消；迟到句柄退出前必须保留准备好的 manifest。
      final lateSpawn = spawning;
      if (lateSpawn != null) unawaited(_collectLateAgent(lateSpawn));
      _launchLog.failed(why: '本地 agent 进程创建超过总启动预算');
      throw const LocalAgentException('本地 agent 进程创建超过总启动预算');
    } on Object catch (e) {
      _agentSpawn = null;
      await _stopComputerUse();
      final message = e is ProcessException ? e.message : '$e';
      _launchLog.failed(why: '启动本地 agent 失败：$message');
      throw LocalAgentException('启动本地 agent 失败：$message');
    }
    _process = proc;
    if (generation != _generation || _stopping) {
      await stop();
      _launchLog.superseded();
      throw const LocalAgentException('本地 agent 进程创建完成时启动已被停止');
    }
    _launchLog.spawn(
      exe: executable,
      pid: proc.pid,
      remote: remote,
      llm: llmRoute ?? 'proxy',
    );

    // Both pipes must be consumed. Not consuming them is a real hang on
    // Windows: the child blocks once the OS buffer fills, which happens after a
    // few hundred log lines — so the agent works fine for a minute and then
    // freezes.
    //
    // They used to be `drain()`ed straight to nothing, which satisfied that
    // requirement and threw away the only record of *why* the child died.
    // Now the last lines are kept; the rest still goes nowhere.
    //
    // Both streams into one buffer: Rust's tracing writes to stdout while a
    // fatal `Error:` goes to stderr, and when diagnosing a death you want the
    // last few log lines *and* the error, interleaved as they happened.
    _pipes.addAll([_tail(proc.stdout), _tail(proc.stderr)]);

    _exitWatch = proc.exitCode.asStream().listen((code) {
      if (!identical(_process, proc)) return;
      final unexpected = !_stopping;
      _process = null;
      _origin = null;
      for (final p in _pipes) {
        unawaited(p.cancel());
      }
      _pipes.clear();
      // 正常退出也记：不记的话，文件里全是崩溃，看不出「这台机器上它平时
      // 活得好好的」，也就判断不了这次崩溃是不是新出现的
      _launchLog.exited(
        code: code,
        expected: !unexpected,
        tail: _logTail.join('\n'),
      );
      // exitCode 已确认 agent 退出；先登记本轮配置清理，再允许监督器重启。
      unawaited(_stopComputerUse());
      if (unexpected) onExit?.call(code, _logTail.join('\n'));
    });

    final addressBudget = remainingStartupBudget();
    if (addressBudget <= Duration.zero) {
      await stop();
      const headline = '本地 agent 启动后总预算已经耗尽';
      _launchLog.failed(why: headline);
      throw LocalAgentException(
        '$headline（${timeout.inSeconds} 秒）\n完整启动记录：$launchLogPath',
      );
    }
    final addr = await _awaitAddress(addrFile, addressBudget);
    // Read once, then drop it — it has served its purpose and leaving it
    // behind is what created the staleness problem in the first place.
    try {
      if (addrFile.existsSync()) addrFile.deleteSync();
    } on FileSystemException {
      // Swept on the next start.
    }
    if (addr == null) {
      // Grab the tail **before** stop(), which clears the pipes.
      //
      // This is the branch a protocol mismatch takes: the child prints one line
      // explaining itself and exits, so `_awaitAddress` returns null and we
      // land here. Reporting only "no address in 20s" would throw away the one
      // sentence that says what to do about it — and that sentence is the
      // entire reason the child refuses to start rather than limping along.
      final why = _logTail.isEmpty ? '' : '\n它最后说的是：\n${_logTail.join('\n')}';
      final died = _process == null;
      final headline = died
          ? '本地 agent 启动后立即退出'
          : '本地 agent 在 ${timeout.inSeconds} 秒内没有报出监听地址';
      // 是**我们自己**在启动中途把它停了（provider 重建、登出、换地址），
      // 那不是启动失败。见 `AgentLaunchLog.superseded` —— 混为一谈的话，
      // 每次开机都会记一条假故障
      if (_stopping) {
        _launchLog.superseded();
      } else {
        _launchLog.failed(why: headline, tail: _logTail.join('\n'));
      }
      await stop();
      // 报错里带上文件路径：这一屏上的字用户复制不走，而崩溃循环里
      // 它每 6 秒被下一次覆盖一遍 —— 能带走的只有那个文件
      throw LocalAgentException('$headline$why\n完整启动记录：$launchLogPath');
    }
    _origin = 'http://$addr';
    _launchLog.ready(
      origin: _origin!,
      ms: DateTime.now().difference(startedAt).inMilliseconds,
    );
    return _origin!;
  }

  /// Polls for this run's handshake file.
  ///
  /// No staleness check is needed — the name is unique per start (see the
  /// comment where it is built). Emptiness is still checked: the child writes
  /// via a temp file and renames, but a zero-byte read would mean the address
  /// is not there yet either way.
  Future<String?> _awaitAddress(File addrFile, Duration timeout) async {
    final deadline = Stopwatch()..start();
    while (deadline.elapsed < timeout) {
      if (_process == null) return null; // died during startup
      if (addrFile.existsSync()) {
        final text = addrFile.readAsStringSync().trim();
        if (text.isNotEmpty) return text;
      }
      final remaining = timeout - deadline.elapsed;
      if (remaining <= Duration.zero) break;
      await Future<void>.delayed(
        remaining < const Duration(milliseconds: 100)
            ? remaining
            : const Duration(milliseconds: 100),
      );
    }
    return null;
  }

  /// Consumes a pipe, keeping the last [_tailLines] lines.
  ///
  /// `allowMalformed` because this decodes whatever the child happened to
  /// write. A stray non-UTF-8 byte — a Windows console codepage leaking through
  /// a library's error string — would otherwise throw *inside the drain*, which
  /// stops consuming the pipe and reintroduces the buffer-full hang. A garbled
  /// character in a log tail is a non-event; a frozen agent is not.
  StreamSubscription<void> _tail(Stream<List<int>> pipe) => pipe
      .transform(const Utf8Decoder(allowMalformed: true))
      .transform(const LineSplitter())
      .listen((line) {
        _logTail.add(line);
        if (_logTail.length > _tailLines) _logTail.removeAt(0);
      }, onError: (_) {});

  /// Removes handshake files left by runs that crashed before reading theirs.
  ///
  /// They are tiny, but an unbounded pile of them in the user's state
  /// directory is the kind of thing that gets noticed years later and looks
  /// like a leak. Best-effort: a failure here must not stop the agent.
  void _sweepStaleHandshakes(Directory dir) {
    // ⚠️ **只扫自己 pid 的那些**，不是所有 `agent-*.addr`。
    //
    // 文件名是 `agent-<本进程pid>-<时间戳>.addr`。扫全部的那一版会跨进程
    // 误杀：另一份桌面端此刻可能正**等着**它自己那个握手文件被子进程写出来
    // （`_awaitAddress` 轮询它），而我们把它删了 —— 对方等到 20 秒超时，
    // 报「本地 agent 在 20 秒内没有报出监听地址」，而它的 agent 其实好好地
    // 起来了、地址也写过了。
    //
    // 一份不该存在的第二实例（见 core/single_instance.dart）本来就该被挡在
    // 门外，但**防护不能建立在另一道防护没漏的前提上** —— 老版本的桌面端
    // 不抢那把锁，而它们至少不该互相删文件。
    //
    // 代价：别的 pid 留下的陈旧文件不再被清。它们是几十字节的空壳，
    // 而清掉别人的东西这件事，风险比那点字节大得多。
    final mine = 'agent-$pid-';
    try {
      if (!dir.existsSync()) return;
      for (final e in dir.listSync()) {
        final name = e.path.split(Platform.pathSeparator).last;
        if (e is File && name.startsWith(mine) && name.endsWith('.addr')) {
          e.deleteSync();
        }
      }
    } on FileSystemException {
      // Not worth failing a start over.
    }
  }

  /// 停止 agent 后才清理本轮配置；重复停止共享同一等待，不能提前删除 manifest。
  /// 未确认退出时保留句柄与文件，配置清理不代表 Rust Job 已排空。
  Future<void> stop() {
    final pending = _agentStopping;
    if (pending != null) return pending;
    late final Future<void> stopping;
    stopping = _stopAgent().whenComplete(() {
      if (identical(_agentStopping, stopping)) _agentStopping = null;
    });
    _agentStopping = stopping;
    return stopping;
  }

  Future<void> _stopAgent() async {
    _stopping = true;
    _generation++;
    _lastCleanupError = null;
    final proc = _process;
    // 在撤掉退出监听**之前**记：撤了之后这次退出就没人看见了。
    // 只有真有进程在跑才记 —— 空 stop（防御性调用）不该在日志里留噪音
    if (proc != null) _launchLog.stopped();
    _origin = null;
    final exitWatch = _exitWatch;
    _exitWatch = null;
    try {
      await exitWatch?.cancel();
    } on Object {
      // 清理监听失败不能让后面的 computer-use 和 agent 进程漏杀；这里已经
      // 撤掉引用，退出通知不会再把这轮有意的关闭误报成崩溃。
    }
    for (final p in _pipes) {
      try {
        await p.cancel();
      } on Object {
        // 单条管道取消失败不能阻断其他管道和主进程的关闭。
      }
    }
    _pipes.clear();
    if (proc == null && _agentSpawn != null) {
      _lastCleanupError = '本地 agent 进程创建尚未返回，保留配置直到迟到句柄退出';
    } else if (proc != null) {
      try {
        proc.kill();
        final code = await proc.exitCode.timeout(
          const Duration(seconds: 3),
          onTimeout: () => _stillRunningExitCode,
        );
        if (code == _stillRunningExitCode) {
          _lastCleanupError = '本地 agent 进程在 3 秒内没有退出，保留配置';
        } else if (identical(_process, proc)) {
          _process = null;
        }
      } on Object catch (error) {
        _lastCleanupError = '本地 agent 退出状态未确认，保留配置：$error';
      }
    }
    if (_lastCleanupError != null) {
      _launchLog.cleanupFailed(
        component: 'local-agent',
        error: _lastCleanupError!,
      );
      return;
    }
    await _stopComputerUse();
  }

  Future<void> _collectLateAgent(Future<Process> spawning) async {
    Process proc;
    try {
      proc = await spawning;
    } on Object {
      await _agentStopping;
      if (identical(_agentSpawn, spawning)) _agentSpawn = null;
      await _stopComputerUse();
      return;
    }
    await _agentStopping;
    if (!identical(_agentSpawn, spawning)) return;
    _agentSpawn = null;
    _process = proc;
    await stop();
  }

  Future<void> _stopComputerUse() {
    final pending = _computerUseStopping;
    final runtime = _computerUse;
    _computerUse = null;
    if (runtime == null) return pending ?? Future<void>.value();

    late final Future<void> stopping;
    stopping = runtime
        .stop()
        .then(
          (_) {
            final error = runtime.lastStopError;
            if (error != null && error.trim().isNotEmpty) {
              _lastCleanupError = error;
              _launchLog.cleanupFailed(component: 'computer-use', error: error);
            }
          },
          onError: (Object error) {
            _lastCleanupError = '$error';
            _launchLog.cleanupFailed(
              component: 'computer-use',
              error: '$error',
            );
          },
        )
        .whenComplete(() {
          if (identical(_computerUseStopping, stopping)) {
            _computerUseStopping = null;
          }
        });
    _computerUseStopping = stopping;
    return stopping;
  }
}

const int _stillRunningExitCode = -2;

class LocalAgentException implements Exception {
  const LocalAgentException(this.message);
  final String message;
  @override
  String toString() => message;
}
