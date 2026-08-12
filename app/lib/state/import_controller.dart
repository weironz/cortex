import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../import/import_source.dart';
import '../models/import_plan.dart';
import 'app_providers.dart';

/// Which step of the import the user is on.
///
/// Modelled as a phase rather than a pile of booleans because the transitions
/// are one-way and the UI must never offer "start" in a state where the bill
/// has not been shown. With booleans, `hasEstimate && !isRunning` is one typo
/// away from a button that spends money without asking.
enum ImportPhase {
  /// Nothing picked yet.
  idle,

  /// The file is being handed over. On Web this is the 97 MB upload.
  preparing,

  /// The bill is on screen, waiting for the user.
  estimated,

  /// Writing.
  running,

  /// Finished — successfully or not; [ImportState.error] tells them apart.
  finished,
}

class ImportState {
  const ImportState({
    this.phase = ImportPhase.idle,
    this.source,
    this.target,
    this.estimate,
    this.progress,
    this.done,
    this.error,
    this.maxConversations,
  });

  final ImportPhase phase;

  /// What the user picked, kept for the filename/size line.
  final ImportSource? source;

  /// What the daemon will parse. Held so the file is handed over **once**.
  final ImportTarget? target;

  final ImportEstimate? estimate;
  final ImportProgressEvent? progress;
  final ImportDoneEvent? done;
  final String? error;

  /// "Only import the newest N conversations." The way to try a few before
  /// committing to thousands of LLM calls.
  final int? maxConversations;

  bool get busy =>
      phase == ImportPhase.preparing || phase == ImportPhase.running;

  ImportState copyWith({
    ImportPhase? phase,
    ImportSource? source,
    ImportTarget? target,
    ImportEstimate? estimate,
    ImportProgressEvent? progress,
    ImportDoneEvent? done,
    Object? error = _keep,
    Object? maxConversations = _keep,
  }) => ImportState(
    phase: phase ?? this.phase,
    source: source ?? this.source,
    target: target ?? this.target,
    estimate: estimate ?? this.estimate,
    progress: progress ?? this.progress,
    done: done ?? this.done,
    error: error == _keep ? this.error : error as String?,
    maxConversations: maxConversations == _keep
        ? this.maxConversations
        : maxConversations as int?,
  );

  static const Object _keep = Object();
}

/// Drives pick → prepare → preview → run.
///
/// **The bill is never skipped.** [start] refuses to do anything unless a
/// preview has already been shown ([ImportPhase.estimated]) — the same rule the
/// CLI encodes by making `--dry-run` the default, and by making preview its own
/// read-only endpoint on both daemons.
class ImportController extends Notifier<ImportState> {
  @override
  ImportState build() => const ImportState();

  StreamSubscription<ImportEvent>? _sub;

  /// Opens the chooser and asks the daemon for the bill.
  ///
  /// One user gesture covers both because there is nothing to decide in
  /// between: picking a file and being told what it would cost is a single
  /// question ("what's in here?"), and splitting it would just add a button.
  ///
  /// [pick] is injectable so tests can drive the **real** transition into
  /// [ImportPhase.estimated] instead of assigning the state directly. That
  /// matters: the rule this class exists to enforce is "no start before the
  /// bill", and a test that hand-places the phase would be checking a value it
  /// wrote itself rather than the path a user actually takes.
  Future<void> pickAndPreview({Future<ImportSource?> Function()? pick}) async {
    final source = await (pick ?? pickExportFile)();
    if (source == null) return;

    state = ImportState(
      phase: ImportPhase.preparing,
      source: source,
      maxConversations: state.maxConversations,
    );
    try {
      final api = ref.read(cortexApiProvider);
      final target = await api.prepareImport(source);
      final estimate = await api.importPreview(
        target,
        maxConversations: state.maxConversations,
      );
      if (!ref.mounted) return;
      state = state.copyWith(
        phase: ImportPhase.estimated,
        target: target,
        estimate: estimate,
        error: null,
      );
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(phase: ImportPhase.idle, error: e.message);
    }
  }

  /// Re-asks for the bill after the user changed "only the newest N".
  ///
  /// Reuses the target, so on Web this costs one small request rather than
  /// another 97 MB.
  Future<void> setMaxConversations(int? max) async {
    state = state.copyWith(maxConversations: max);
    final target = state.target;
    if (target == null || state.phase == ImportPhase.running) return;
    try {
      final estimate = await ref
          .read(cortexApiProvider)
          .importPreview(target, maxConversations: max);
      if (!ref.mounted) return;
      state = state.copyWith(estimate: estimate, error: null);
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(error: e.message);
    }
  }

  /// Actually writes. **Only reachable once the bill has been shown.**
  void start() {
    final target = state.target;
    if (target == null || state.phase != ImportPhase.estimated) return;

    state = state.copyWith(
      phase: ImportPhase.running,
      error: null,
      progress: null,
      done: null,
    );
    _sub?.cancel();
    _sub = ref
        .read(cortexApiProvider)
        .runImport(target, maxConversations: state.maxConversations)
        .listen(
          _onEvent,
          onError: (Object e) {
            if (!ref.mounted) return;
            state = state.copyWith(
              phase: ImportPhase.finished,
              error: e is CortexApiException ? e.message : '$e',
            );
          },
          // A stream that ends without a `done` frame means the connection
          // dropped mid-import. Landing in `finished` with no summary would
          // read as success; say what actually happened instead.
          onDone: () {
            if (!ref.mounted) return;
            if (state.phase == ImportPhase.running) {
              state = state.copyWith(
                phase: ImportPhase.finished,
                error:
                    '连接在导入过程中断开了。已经写进去的不会丢，'
                    '重新导入同一个文件即可续上 —— 服务端按 id 判重，不会重复计费。',
              );
            }
          },
        );
  }

  void _onEvent(ImportEvent event) {
    if (!ref.mounted) return;
    switch (event) {
      // The daemon recomputes the bill at the moment work starts. Trust that
      // one over the preview: the preview may be minutes old.
      case ImportStartedEvent(:final estimate):
        state = state.copyWith(estimate: estimate);
      case ImportProgressEvent():
        state = state.copyWith(progress: event);
      case ImportDoneEvent():
        state = state.copyWith(phase: ImportPhase.finished, done: event);
      case ImportErrorEvent(:final message):
        state = state.copyWith(phase: ImportPhase.finished, error: message);
    }
  }

  /// Back to square one. Does **not** cancel a run in flight — see [cancel].
  void reset() {
    _sub?.cancel();
    _sub = null;
    state = const ImportState();
  }

  /// Stops listening.
  ///
  /// Deliberately named `cancel` and not `abort`: the daemon keeps going, and
  /// saying otherwise would be a lie. Dropping the SSE connection only stops
  /// the progress bar — which is why the message says a rerun is safe rather
  /// than pretending the work was undone.
  void cancel() {
    _sub?.cancel();
    _sub = null;
    if (state.phase == ImportPhase.running) {
      state = state.copyWith(
        phase: ImportPhase.finished,
        error:
            '已停止跟踪进度。**服务端那边还在继续跑** —— '
            '想知道结果就重新导入同一个文件，已经写过的会被跳过。',
      );
    }
  }
}

final importControllerProvider =
    NotifierProvider<ImportController, ImportState>(ImportController.new);
