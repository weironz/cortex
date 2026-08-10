import 'package:file_picker/file_picker.dart';

import 'import_source.dart';

/// Desktop: take the **path** and leave the bytes where they are.
///
/// `withData: false` is the whole point of this file. The default on desktop is
/// already path-only, but saying it explicitly matters here: flipping it would
/// pull 97 MB into the Flutter isolate for no reason at all — the process that
/// actually parses the file is the local agent, and it can open the path itself.
///
/// Returns `null` when the user cancels the chooser.
Future<ImportSource?> pickExportFile() async {
  final result = await FilePicker.pickFiles(
    withData: false,
    dialogTitle: '选择导出包里的 conversations.json',
    // Not restricted to `.json`: Claude and ChatGPT both ship the file with
    // that extension today, but a filter that silently hides the right file is
    // worse than a filter that catches nothing. The parser fails loudly and
    // prints the keys it actually saw.
  );
  final file = result?.files.singleOrNull;
  final path = file?.path;
  if (file == null || path == null) return null;
  return ImportPath(path: path, filename: file.name, sizeBytes: file.size);
}
