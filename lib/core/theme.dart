import 'package:flutter/material.dart';
import 'package:google_fonts/google_fonts.dart';

/// The Graphite + Twitter Blue palette from Design.md §2.
///
/// Neomorphism only works when background and surface share one hue — depth
/// comes entirely from the shadow pair, so every value here is a tint of the
/// same graphite rather than an independent colour choice.
abstract final class VonColors {
  static const bgPrimary = Color(0xFF222226);
  static const bgSurface = Color(0xFF2A2A2E);
  static const bgElevated = Color(0xFF303034);
  static const shadowDark = Color(0xFF161618);
  static const shadowLight = Color(0xFF343438);

  static const accent = Color(0xFF1DA1F2);
  static const accentGlow = Color(0x4D1DA1F2); // 30% opacity
  static const accentText = Color(0xFF4DB8FF);

  static const textPrimary = Color(0xFFE0E0E0);
  static const textSecondary = Color(0xFF9A9A9E);
  static const textDisabled = Color(0xFF555558);

  /// Not in the base spec, but the governor needs to say "warm" and "hot"
  /// without borrowing the accent, which means "active" everywhere else.
  static const warn = Color(0xFFE0A458);
  static const danger = Color(0xFFE05A5A);
  static const success = Color(0xFF57C98A);
}

/// The squircle radii scale from Design.md §4.
abstract final class VonRadius {
  static const small = 8.0;
  static const medium = 14.0;
  static const large = 24.0;
  static const extraLarge = 32.0;
}

abstract final class VonSpace {
  static const xs = 4.0;
  static const sm = 8.0;
  static const md = 16.0;
  static const lg = 24.0;
  static const xl = 32.0;
}

/// Motion constants from Design.md §7. Presses are linear-feeling easing, not
/// spring physics — the illusion is rigid plastic, and a spring reads as rubber.
abstract final class VonMotion {
  static const press = Duration(milliseconds: 150);
  static const pressCurve = Curves.easeInOut;

  static const sheet = Duration(milliseconds: 320);
  static const sheetCurve = Curves.easeOutBack;

  static const cursorBlink = Duration(milliseconds: 500);
  static const shimmer = Duration(milliseconds: 1600);
}

abstract final class VonText {
  static TextStyle get display => GoogleFonts.inter(
        fontSize: 28,
        fontWeight: FontWeight.w600,
        color: VonColors.textPrimary,
        letterSpacing: -0.5,
      );

  static TextStyle get title => GoogleFonts.inter(
        fontSize: 20,
        fontWeight: FontWeight.w600,
        color: VonColors.textPrimary,
        letterSpacing: -0.2,
      );

  static TextStyle get heading => GoogleFonts.inter(
        fontSize: 16,
        fontWeight: FontWeight.w600,
        color: VonColors.textPrimary,
      );

  static TextStyle get body => GoogleFonts.inter(
        fontSize: 14,
        fontWeight: FontWeight.w400,
        color: VonColors.textPrimary,
        height: 1.5,
      );

  static TextStyle get secondary => GoogleFonts.inter(
        fontSize: 13,
        fontWeight: FontWeight.w400,
        color: VonColors.textSecondary,
        height: 1.45,
      );

  static TextStyle get caption => GoogleFonts.inter(
        fontSize: 11,
        fontWeight: FontWeight.w500,
        color: VonColors.textSecondary,
        letterSpacing: 0.3,
      );

  static TextStyle get button => GoogleFonts.inter(
        fontSize: 14,
        fontWeight: FontWeight.w700,
        color: VonColors.accent,
        letterSpacing: 0.2,
      );

  /// JetBrains Mono for anything the agent produced — code, terminal output,
  /// file paths. The weight split (400 vs 500) distinguishes literal output
  /// from the agent's own reasoning.
  static TextStyle get code => GoogleFonts.jetBrainsMono(
        fontSize: 12.5,
        fontWeight: FontWeight.w400,
        color: VonColors.textPrimary,
        height: 1.5,
      );

  static TextStyle get thought => GoogleFonts.jetBrainsMono(
        fontSize: 12,
        fontWeight: FontWeight.w500,
        color: VonColors.textSecondary,
        height: 1.5,
      );

  static TextStyle get mono => GoogleFonts.jetBrainsMono(
        fontSize: 11.5,
        fontWeight: FontWeight.w400,
        color: VonColors.textSecondary,
      );
}

ThemeData buildVonTheme() {
  final base = ThemeData.dark(useMaterial3: true);

  return base.copyWith(
    scaffoldBackgroundColor: VonColors.bgPrimary,
    canvasColor: VonColors.bgPrimary,
    colorScheme: base.colorScheme.copyWith(
      surface: VonColors.bgSurface,
      primary: VonColors.accent,
      secondary: VonColors.accentText,
      error: VonColors.danger,
      onSurface: VonColors.textPrimary,
    ),
    textTheme: base.textTheme.apply(
      bodyColor: VonColors.textPrimary,
      displayColor: VonColors.textPrimary,
      fontFamily: GoogleFonts.inter().fontFamily,
    ),
    // Material's ink splash fights the neomorphic press animation — two
    // different affordances firing on the same tap. Our own press handles it.
    splashFactory: NoSplash.splashFactory,
    highlightColor: Colors.transparent,
    splashColor: Colors.transparent,
    appBarTheme: const AppBarTheme(
      backgroundColor: VonColors.bgPrimary,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      scrolledUnderElevation: 0,
    ),
    dividerTheme: const DividerThemeData(
      color: VonColors.shadowDark,
      thickness: 1,
      space: 1,
    ),
    // Neomorphism has no borders to tint, so the text cursor and selection are
    // the only places the accent shows inside an input.
    textSelectionTheme: const TextSelectionThemeData(
      cursorColor: VonColors.accent,
      selectionColor: VonColors.accentGlow,
      selectionHandleColor: VonColors.accent,
    ),
  );
}
