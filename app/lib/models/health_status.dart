import 'json.dart';

/// `GET /health` → `{"status":"ok","version":"0.0.1","database":"ok"}`
class HealthStatus {
  const HealthStatus({
    required this.status,
    required this.version,
    required this.database,
  });

  final String status;
  final String version;
  final String database;

  /// Only the process itself has to be up for the client to be usable.
  ///
  /// Deliberately does **not** require `database == 'ok'`: during M2/M5
  /// development the daemon legitimately reports `not_wired`, and the chat and
  /// memory routes still answer. Storage state is surfaced separately by
  /// [databaseNote] rather than turning the whole UI red.
  bool get isHealthy => status == 'ok';

  /// Null when storage is fully wired; otherwise a short note for the status
  /// strip.
  String? get databaseNote => switch (database) {
    'ok' => null,
    'not_wired' => '存储层未接线',
    final other => '存储: $other',
  };

  factory HealthStatus.fromJson(Map<String, dynamic> json) => HealthStatus(
    status: asString(json['status'], 'unknown'),
    version: asString(json['version'], '?'),
    database: asString(json['database'], 'unknown'),
  );
}
