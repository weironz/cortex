import 'package:intl/intl.dart';

final _timeOnly = DateFormat('HH:mm');
final _monthDay = DateFormat('M月d日 HH:mm');
final _full = DateFormat('yyyy年M月d日 HH:mm');
final _dateOnly = DateFormat('yyyy-MM-dd');

/// Sidebar / list timestamps: relative for anything recent, absolute beyond.
String formatRelative(DateTime? when) {
  if (when == null) return '';
  final now = DateTime.now();
  final diff = now.difference(when);

  if (diff.inSeconds < 60) return '刚刚';
  if (diff.inMinutes < 60) return '${diff.inMinutes} 分钟前';
  if (diff.inHours < 24 && now.day == when.day) {
    return _timeOnly.format(when);
  }
  if (diff.inDays < 2) return '昨天 ${_timeOnly.format(when)}';
  if (diff.inDays < 365) return _monthDay.format(when);
  return _full.format(when);
}

/// Provenance timestamps, where precision matters more than brevity.
String formatAbsolute(DateTime? when) =>
    when == null ? '时间未知' : _full.format(when);

String formatDate(DateTime? when) =>
    when == null ? '—' : _dateOnly.format(when);

String formatConfidence(double? c) =>
    c == null ? '' : '${(c * 100).round()}%';

/// Shortens a ULID for display: `01KZC66ZT7…R6C`.
String shortId(String? id, {int head = 8, int tail = 4}) {
  if (id == null || id.isEmpty) return '—';
  if (id.length <= head + tail + 1) return id;
  return '${id.substring(0, head)}…${id.substring(id.length - tail)}';
}
