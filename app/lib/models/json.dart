/// Lenient JSON coercion helpers.
///
/// The backend is still in flux (M5 is being built in parallel), so the client
/// deliberately tolerates missing/loosely-typed fields instead of throwing.
/// Anything genuinely required is validated at the call site.
library;

String asString(Object? v, [String fallback = '']) =>
    v is String ? v : (v == null ? fallback : v.toString());

String? asStringOrNull(Object? v) {
  if (v == null) return null;
  final s = v is String ? v : v.toString();
  return s.isEmpty ? null : s;
}

double? asDoubleOrNull(Object? v) => switch (v) {
  final double d => d,
  final int i => i.toDouble(),
  final String s => double.tryParse(s),
  _ => null,
};

int asInt(Object? v, [int fallback = 0]) => switch (v) {
  final int i => i,
  final double d => d.toInt(),
  final String s => int.tryParse(s) ?? fallback,
  _ => fallback,
};

DateTime? asDateOrNull(Object? v) {
  if (v == null) return null;
  if (v is DateTime) return v;
  if (v is int) return DateTime.fromMillisecondsSinceEpoch(v, isUtc: true);
  if (v is String && v.isNotEmpty) return DateTime.tryParse(v)?.toLocal();
  return null;
}

/// Reads a list of non-empty strings, tolerating `null` and non-list values.
List<String> asStringList(Object? v) {
  if (v is! List) return const [];
  return v
      .map(asString)
      .where((e) => e.isNotEmpty)
      .toList(growable: false);
}

/// Reads `json[key]` as a list of objects, tolerating `null` and non-list
/// values (returns an empty list rather than throwing).
List<Map<String, dynamic>> asObjectList(Object? v) {
  if (v is! List) return const [];
  return v.whereType<Map<String, dynamic>>().toList(growable: false);
}
