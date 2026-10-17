part of '../cortex_api.dart';

mixin CortexApiImport {
  /// Hands the daemon a file to parse, and gets back something cheap to refer
  /// to it by.
  ///
  /// Desktop passes a path straight through — the local agent opens it itself,
  /// so nothing moves. Web uploads via `POST /import/upload` and gets a spool
  /// handle. **This is the only place the 97 MB travels**; preview and run both
  /// take the handle, so the file is sent once rather than once per step.
  Future<ImportTarget> prepareImport(ImportSource source);

  /// The bill. Reads the file, writes nothing.
  ///
  /// A separate endpoint from [runImport] rather than a `dryRun: true` flag, so
  /// that calling the wrong one cannot cost money: a read-only endpoint has no
  /// path to a write. Every pair in the estimate is one LLM call, and memory is
  /// append-only — undoing an import means `redact`, which is deliberately
  /// explicit and confirmed twice.
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  });

  /// Does it. Emits [ImportStartedEvent], then [ImportProgressEvent]s, then
  /// exactly one [ImportDoneEvent] or [ImportErrorEvent].
  ///
  /// Safe to rerun: episode ids are derived from (original timestamp, stable
  /// seed) and the daemon dedupes on them, so a second run only fills gaps and
  /// re-bills nothing.
  Stream<ImportEvent> runImport(ImportTarget target, {int? maxConversations});
}
