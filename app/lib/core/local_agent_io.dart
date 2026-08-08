import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// Desktop can run an agent. See `local_agent.dart` for the seam's rationale.
const bool kLocalAgentSupported = true;

/// Environment override for development.
///
/// `flutter run` puts the app's exe in a build directory that has no
/// `cortex-local` next to it, so discovery would find nothing and the whole
/// local path would silently never be exercised until packaging.
const String kAgentPathEnvVar = 'CORTEX_LOCAL_BIN';

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
  final state = _stateDir();
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
String? _stateDir() {
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
  LocalAgent({required this.executable, required this.stateDir});

  /// Absolute path to `cortex-local` (`.exe` on Windows).
  final String executable;

  /// Where the address handshake file goes. Same directory the agent already
  /// uses for its outbox and workspace bindings.
  final String stateDir;

  Process? _process;
  String? _origin;
  StreamSubscription<void>? _exitWatch;
  final List<StreamSubscription<void>> _pipes = [];

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
    Duration timeout = const Duration(seconds: 20),
    void Function(int code, String logTail)? onExit,
  }) async {
    if (_process != null) return _origin!;
    _stopping = false;
    _logTail.clear();

    final exe = File(executable);
    if (!exe.existsSync()) {
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
    final addrFile =
        File('$stateDir${Platform.pathSeparator}agent-$pid-$stamp.addr');
    _sweepStaleHandshakes(Directory(stateDir));

    final Process proc;
    try {
      proc = await Process.start(executable, [
        '--bind',
        // Port 0: the kernel picks. See the class doc.
        '127.0.0.1:0',
        '--remote',
        remote,
        '--parent-pid',
        '$pid',
        '--addr-file',
        addrFile.path,
      ], environment: {
        // The token travels in the environment, never on argv: command lines
        // are visible to every other process on the machine (`tasklist /v`,
        // `ps aux`) and get captured by crash reporters.
        'CORTEX_TOKEN': token,
      });
    } on ProcessException catch (e) {
      throw LocalAgentException('启动本地 agent 失败：${e.message}');
    }
    _process = proc;

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
      final unexpected = !_stopping;
      _process = null;
      _origin = null;
      for (final p in _pipes) {
        unawaited(p.cancel());
      }
      _pipes.clear();
      if (unexpected) onExit?.call(code, _logTail.join('\n'));
    });

    final addr = await _awaitAddress(addrFile, timeout);
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
      await stop();
      throw LocalAgentException(
        died
            ? '本地 agent 启动后立即退出$why'
            : '本地 agent 在 ${timeout.inSeconds} 秒内没有报出监听地址$why',
      );
    }
    _origin = 'http://$addr';
    return _origin!;
  }

  /// Polls for this run's handshake file.
  ///
  /// No staleness check is needed — the name is unique per start (see the
  /// comment where it is built). Emptiness is still checked: the child writes
  /// via a temp file and renames, but a zero-byte read would mean the address
  /// is not there yet either way.
  Future<String?> _awaitAddress(File addrFile, Duration timeout) async {
    final deadline = DateTime.now().add(timeout);
    while (DateTime.now().isBefore(deadline)) {
      if (_process == null) return null; // died during startup
      if (addrFile.existsSync()) {
        final text = addrFile.readAsStringSync().trim();
        if (text.isNotEmpty) return text;
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
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
    try {
      if (!dir.existsSync()) return;
      for (final e in dir.listSync()) {
        final name = e.path.split(Platform.pathSeparator).last;
        if (e is File && name.startsWith('agent-') && name.endsWith('.addr')) {
          e.deleteSync();
        }
      }
    } on FileSystemException {
      // Not worth failing a start over.
    }
  }

  /// Stops the agent. Safe to call when it is not running.
  ///
  /// Sets [_stopping] first so the exit this causes is not reported through
  /// `onExit` — otherwise signing out would look like a crash and the
  /// supervisor would helpfully restart the process we just killed.
  Future<void> stop() async {
    _stopping = true;
    final proc = _process;
    _process = null;
    _origin = null;
    await _exitWatch?.cancel();
    _exitWatch = null;
    for (final p in _pipes) {
      await p.cancel();
    }
    _pipes.clear();
    if (proc == null) return;
    proc.kill();
    // Bounded wait: if it will not go, we are on our way out anyway and the
    // child's own parent-watch will collect it within a couple of seconds.
    await proc.exitCode.timeout(
      const Duration(seconds: 3),
      onTimeout: () => -1,
    );
  }
}

class LocalAgentException implements Exception {
  const LocalAgentException(this.message);
  final String message;
  @override
  String toString() => message;
}
