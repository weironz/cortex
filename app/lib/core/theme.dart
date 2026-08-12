import 'package:flutter/material.dart';

/// Cortex design tokens.
///
/// Deliberately hand-built rather than `ColorScheme.fromSeed`: the generated
/// tonal palette gives every surface a visible hue cast, which reads as "a
/// Flutter demo". Product surfaces here are near-neutral (slightly cool greys)
/// and the single indigo accent is reserved for things that are actually
/// interactive or actually memory-related.
abstract final class CortexTheme {
  // Accent — used for the user bubble, focus rings, primary actions.
  static const _indigo = Color(0xFF5B62F4);
  static const _indigoLight = Color(0xFF7A80FF);

  // Memory affordances get their own hue so provenance never competes with
  // primary actions for attention.
  static const memoryAccentLight = Color(0xFF0E7C86);
  static const memoryAccentDark = Color(0xFF4FD1C5);

  static ThemeData light() => _build(_lightScheme, Brightness.light);
  static ThemeData dark() => _build(_darkScheme, Brightness.dark);

  static const _lightScheme = ColorScheme(
    brightness: Brightness.light,
    primary: _indigo,
    onPrimary: Colors.white,
    primaryContainer: Color(0xFFE4E5FF),
    onPrimaryContainer: Color(0xFF1B1D66),
    secondary: memoryAccentLight,
    onSecondary: Colors.white,
    secondaryContainer: Color(0xFFD3F1F3),
    onSecondaryContainer: Color(0xFF04353A),
    error: Color(0xFFB3261E),
    onError: Colors.white,
    errorContainer: Color(0xFFF9DEDC),
    onErrorContainer: Color(0xFF410E0B),
    surface: Color(0xFFFCFCFD),
    onSurface: Color(0xFF1A1C1F),
    onSurfaceVariant: Color(0xFF5A5F66),
    outline: Color(0xFFC3C6CF),
    outlineVariant: Color(0xFFE3E5EA),
    surfaceContainerLowest: Colors.white,
    surfaceContainerLow: Color(0xFFF7F8FA),
    surfaceContainer: Color(0xFFF2F3F6),
    surfaceContainerHigh: Color(0xFFECEEF2),
    surfaceContainerHighest: Color(0xFFE6E8EE),
    inverseSurface: Color(0xFF2E3033),
    onInverseSurface: Color(0xFFF1F1F3),
  );

  static const _darkScheme = ColorScheme(
    brightness: Brightness.dark,
    primary: _indigoLight,
    onPrimary: Color(0xFF10123D),
    primaryContainer: Color(0xFF32357F),
    onPrimaryContainer: Color(0xFFE0E1FF),
    secondary: memoryAccentDark,
    onSecondary: Color(0xFF00312F),
    secondaryContainer: Color(0xFF0C4A4C),
    onSecondaryContainer: Color(0xFFB8F0EC),
    error: Color(0xFFF2B8B5),
    onError: Color(0xFF601410),
    errorContainer: Color(0xFF8C1D18),
    onErrorContainer: Color(0xFFF9DEDC),
    surface: Color(0xFF101114),
    onSurface: Color(0xFFE4E5E9),
    onSurfaceVariant: Color(0xFF9CA1AC),
    outline: Color(0xFF3A3D45),
    outlineVariant: Color(0xFF2A2D34),
    surfaceContainerLowest: Color(0xFF0B0C0E),
    surfaceContainerLow: Color(0xFF15171A),
    surfaceContainer: Color(0xFF1A1C20),
    surfaceContainerHigh: Color(0xFF212429),
    surfaceContainerHighest: Color(0xFF292C32),
    inverseSurface: Color(0xFFE4E5E9),
    onInverseSurface: Color(0xFF1A1C20),
  );

  static ThemeData _build(ColorScheme scheme, Brightness brightness) {
    final base = ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      brightness: brightness,
    );

    return base.copyWith(
      scaffoldBackgroundColor: scheme.surface,
      dividerTheme: DividerThemeData(
        color: scheme.outlineVariant,
        thickness: 1,
        space: 1,
      ),
      textTheme: _textTheme(base.textTheme, scheme),
      inputDecorationTheme: InputDecorationTheme(
        filled: true,
        fillColor: scheme.surfaceContainerHigh,
        hintStyle: TextStyle(color: scheme.onSurfaceVariant),
        contentPadding: const EdgeInsets.symmetric(
          horizontal: 14,
          vertical: 12,
        ),
        border: _inputBorder(scheme.outlineVariant),
        enabledBorder: _inputBorder(Colors.transparent),
        focusedBorder: _inputBorder(scheme.primary, width: 1.5),
        isDense: true,
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: FilledButton.styleFrom(
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(10),
          ),
          padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 14),
          textStyle: const TextStyle(fontWeight: FontWeight.w600),
        ),
      ),
      iconButtonTheme: IconButtonThemeData(
        style: IconButton.styleFrom(
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(9)),
        ),
      ),
      tooltipTheme: TooltipThemeData(
        waitDuration: const Duration(milliseconds: 500),
        decoration: BoxDecoration(
          color: scheme.inverseSurface,
          borderRadius: BorderRadius.circular(6),
        ),
        textStyle: TextStyle(color: scheme.onInverseSurface, fontSize: 12),
      ),
      chipTheme: ChipThemeData(
        side: BorderSide(color: scheme.outlineVariant),
        backgroundColor: scheme.surfaceContainer,
        labelStyle: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 0),
      ),
      scrollbarTheme: ScrollbarThemeData(
        thickness: const WidgetStatePropertyAll(8),
        radius: const Radius.circular(4),
        thumbColor: WidgetStatePropertyAll(
          scheme.onSurfaceVariant.withValues(alpha: 0.35),
        ),
      ),
      splashFactory: InkSparkle.splashFactory,
    );
  }

  static OutlineInputBorder _inputBorder(Color color, {double width = 1}) =>
      OutlineInputBorder(
        borderRadius: BorderRadius.circular(10),
        borderSide: BorderSide(color: color, width: width),
      );

  static TextTheme _textTheme(TextTheme base, ColorScheme scheme) => base
      .copyWith(
        titleLarge: base.titleLarge?.copyWith(
          fontSize: 17,
          fontWeight: FontWeight.w600,
          letterSpacing: -0.2,
        ),
        titleMedium: base.titleMedium?.copyWith(
          fontSize: 14.5,
          fontWeight: FontWeight.w600,
        ),
        titleSmall: base.titleSmall?.copyWith(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.4,
        ),
        bodyLarge: base.bodyLarge?.copyWith(fontSize: 15, height: 1.62),
        bodyMedium: base.bodyMedium?.copyWith(fontSize: 14, height: 1.6),
        bodySmall: base.bodySmall?.copyWith(
          fontSize: 12.5,
          height: 1.5,
          color: scheme.onSurfaceVariant,
        ),
        labelSmall: base.labelSmall?.copyWith(
          fontSize: 11,
          letterSpacing: 0.3,
          color: scheme.onSurfaceVariant,
        ),
      )
      .apply(fontFamilyFallback: _cjkFallback);

  /// Flutter's default `Roboto` has no CJK coverage; without an explicit
  /// fallback chain, Chinese renders as tofu on Windows in some locales and
  /// picks an inconsistent face on Web.
  static const _cjkFallback = <String>[
    'Microsoft YaHei UI',
    'Microsoft YaHei',
    'PingFang SC',
    'Hiragino Sans GB',
    'Noto Sans CJK SC',
    'Source Han Sans SC',
    'sans-serif',
  ];

  /// Monospace stack for code blocks and ids.
  static const monoFallback = <String>[
    'JetBrains Mono',
    'Cascadia Code',
    'Consolas',
    'SF Mono',
    'Menlo',
    'DejaVu Sans Mono',
    'monospace',
  ];
}
