import 'package:file_picker/file_picker.dart';

import 'import_source.dart';

/// Web: there is no path a daemon could open, so the bytes have to come along.
///
/// `withData: true` is the browser's only mode anyway — stated explicitly
/// because this is the expensive branch and it should be visible in the diff
/// that it is expensive. A 97 MB export becomes a 97 MB `Uint8List` in the tab,
/// and then a 97 MB upload.
///
/// The size is surfaced to the caller ([ImportSource.sizeBytes]) precisely so
/// the UI can say what it is about to send before it sends it.
///
/// Returns `null` when the user cancels the chooser.
Future<ImportSource?> pickExportFile() async {
  final result = await FilePicker.pickFiles(
    withData: true,
    dialogTitle: '选择导出包里的 conversations.json',
  );
  final file = result?.files.singleOrNull;
  final bytes = file?.bytes;
  if (file == null || bytes == null) return null;
  return ImportBytes(bytes: bytes, filename: file.name, sizeBytes: file.size);
}
