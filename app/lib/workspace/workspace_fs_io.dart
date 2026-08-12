import 'dart:io';

import 'package:file_picker/file_picker.dart';

import '../models/workspace.dart';

/// Desktop and mobile can read the local disk.
const bool kCanBrowseLocalFiles = true;

/// Why the Web build cannot. Unused here; kept so both sides of the seam expose
/// the same names and a caller never has to know which one it linked against.
const String kNoLocalFilesReason = '';

/// Opens the OS directory chooser. Returns null when the user cancels.
Future<String?> pickWorkspaceDirectory() =>
    FilePicker.getDirectoryPath(dialogTitle: '选择工作区目录');

/// True when [path] exists and is a directory.
///
/// Used before rendering the file tree because a workspace binding **syncs
/// across devices while the directory does not**: the daemon stores an absolute
/// local path, and the same path almost certainly does not exist on the user's
/// other machine. The contract says such a session should read as unbound
/// rather than as broken.
Future<bool> workspaceExists(String path) => Directory(path).exists();

/// One level of [path], directories first then files, both alphabetical.
///
/// Deliberately not recursive: a `node_modules` or `target` directory would
/// otherwise stall the first paint for seconds. The tree expands on demand.
///
/// Entries that cannot be stat'd (permission denied, a symlink to nowhere, a
/// file deleted between listing and stat) are skipped rather than thrown on —
/// one unreadable entry must not blank the whole panel.
Future<List<FileNode>> listWorkspaceDirectory(String path) async {
  final dir = Directory(path);
  if (!await dir.exists()) {
    throw FileSystemException('目录不存在', path);
  }

  final directories = <FileNode>[];
  final files = <FileNode>[];

  await for (final entity in dir.list(followLinks: false)) {
    final name = _basename(entity.path);
    // Dotfiles are hidden by default; a workspace root is mostly `.git`,
    // `.idea`, `.dart_tool` noise otherwise.
    if (name.startsWith('.')) continue;
    try {
      final stat = await entity.stat();
      if (stat.type == FileSystemEntityType.directory) {
        directories.add(
          FileNode(name: name, path: entity.path, isDirectory: true),
        );
      } else if (stat.type == FileSystemEntityType.file) {
        files.add(
          FileNode(
            name: name,
            path: entity.path,
            isDirectory: false,
            sizeBytes: stat.size,
          ),
        );
      }
    } on FileSystemException {
      continue;
    }
  }

  directories.sort((a, b) => _compare(a.name, b.name));
  files.sort((a, b) => _compare(a.name, b.name));
  return [...directories, ...files];
}

int _compare(String a, String b) => a.toLowerCase().compareTo(b.toLowerCase());

String _basename(String path) {
  final i = path.lastIndexOf(RegExp(r'[/\\]'));
  return i < 0 ? path : path.substring(i + 1);
}
