import 'package:flutter/material.dart';
import 'package:re_highlight/languages/bash.dart';
import 'package:re_highlight/languages/c.dart';
import 'package:re_highlight/languages/cpp.dart';
import 'package:re_highlight/languages/csharp.dart';
import 'package:re_highlight/languages/css.dart';
import 'package:re_highlight/languages/dart.dart';
import 'package:re_highlight/languages/diff.dart';
import 'package:re_highlight/languages/dockerfile.dart';
import 'package:re_highlight/languages/go.dart';
import 'package:re_highlight/languages/ini.dart';
import 'package:re_highlight/languages/java.dart';
import 'package:re_highlight/languages/javascript.dart';
import 'package:re_highlight/languages/json.dart';
import 'package:re_highlight/languages/kotlin.dart';
import 'package:re_highlight/languages/markdown.dart';
import 'package:re_highlight/languages/php.dart';
import 'package:re_highlight/languages/plaintext.dart';
import 'package:re_highlight/languages/python.dart';
import 'package:re_highlight/languages/ruby.dart';
import 'package:re_highlight/languages/rust.dart';
import 'package:re_highlight/languages/sql.dart';
import 'package:re_highlight/languages/swift.dart';
import 'package:re_highlight/languages/typescript.dart';
import 'package:re_highlight/languages/xml.dart';
import 'package:re_highlight/languages/yaml.dart';
import 'package:re_highlight/re_highlight.dart';
import 'package:re_highlight/styles/atom-one-dark.dart';
import 'package:re_highlight/styles/github.dart';

/// Shared syntax-highlighting engine.
///
/// Languages are registered individually rather than via
/// `re_highlight/languages/all.dart`. `all.dart` pulls in ~190 grammars; each
/// is a const Dart structure that tree-shaking cannot drop once referenced by
/// the `builtinAllLanguages` map, which adds roughly a megabyte to the release
/// web bundle for languages an agent client will never emit. This curated set
/// covers what Cortex actually talks about, and unknown fences degrade to
/// plain-but-styled text rather than failing.
abstract final class HighlightRegistry {
  static final Highlight _engine = _create();

  static Highlight _create() {
    final h = Highlight()
      ..registerLanguages({
        'bash': langBash,
        'c': langC,
        'cpp': langCpp,
        'csharp': langCsharp,
        'css': langCss,
        'dart': langDart,
        'diff': langDiff,
        'dockerfile': langDockerfile,
        'go': langGo,
        'ini': langIni,
        'java': langJava,
        'javascript': langJavascript,
        'json': langJson,
        'kotlin': langKotlin,
        'markdown': langMarkdown,
        'php': langPhp,
        'plaintext': langPlaintext,
        'python': langPython,
        'ruby': langRuby,
        'rust': langRust,
        'sql': langSql,
        'swift': langSwift,
        'typescript': langTypescript,
        'xml': langXml,
        'yaml': langYaml,
      });
    return h;
  }

  /// Fence-info aliases → registered grammar names.
  static const _aliases = <String, String>{
    'sh': 'bash',
    'shell': 'bash',
    'zsh': 'bash',
    'console': 'bash',
    'ps1': 'bash',
    'powershell': 'bash',
    'js': 'javascript',
    'jsx': 'javascript',
    'mjs': 'javascript',
    'node': 'javascript',
    'ts': 'typescript',
    'tsx': 'typescript',
    'py': 'python',
    'python3': 'python',
    'rs': 'rust',
    'golang': 'go',
    'c++': 'cpp',
    'cc': 'cpp',
    'h': 'cpp',
    'hpp': 'cpp',
    'cs': 'csharp',
    'kt': 'kotlin',
    'kts': 'kotlin',
    'rb': 'ruby',
    'yml': 'yaml',
    'toml': 'ini',
    'cfg': 'ini',
    'conf': 'ini',
    'html': 'xml',
    'svg': 'xml',
    'vue': 'xml',
    'md': 'markdown',
    'markdown': 'markdown',
    'patch': 'diff',
    'text': 'plaintext',
    'txt': 'plaintext',
    '': 'plaintext',
  };

  /// Normalises a fence info string (`” ```rust,no_run ”` → `rust`).
  static String? resolveLanguage(String? raw) {
    if (raw == null) return null;
    var name = raw.trim().toLowerCase();
    if (name.isEmpty) return null;
    // Strip attribute suffixes some doc toolchains attach.
    for (final sep in const [' ', ',', ':', '{']) {
      final i = name.indexOf(sep);
      if (i > 0) name = name.substring(0, i);
    }
    final resolved = _aliases[name] ?? name;
    return _engine.getLanguage(resolved) != null ? resolved : null;
  }

  /// Highlights [code], returning null when the language is unknown or the
  /// block is large enough that highlighting on every streaming delta would
  /// cost more than it is worth.
  static TextSpan? highlight({
    required String code,
    required String? language,
    required TextStyle baseStyle,
    required Brightness brightness,
  }) {
    final lang = resolveLanguage(language);
    if (lang == null || lang == 'plaintext') return null;
    if (code.length > _maxHighlightedChars) return null;

    try {
      final result = _engine.highlight(code: code, language: lang);
      final renderer = TextSpanRenderer(baseStyle, themeFor(brightness));
      result.render(renderer);
      return renderer.span;
    } on Object {
      // A grammar that chokes on half-typed code mid-stream must never take
      // the message down; fall back to unstyled text for this frame.
      return null;
    }
  }

  /// Above this, the regex engine's per-frame cost during streaming becomes
  /// noticeable; such blocks render unhighlighted.
  static const _maxHighlightedChars = 20000;

  static Map<String, TextStyle> themeFor(Brightness brightness) =>
      brightness == Brightness.dark ? atomOneDarkTheme : githubTheme;

  /// Display label for the code block header.
  static String labelFor(String? raw) {
    final trimmed = (raw ?? '').trim();
    if (trimmed.isEmpty) return 'text';
    return trimmed;
  }
}
