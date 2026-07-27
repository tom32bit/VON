import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_highlight/flutter_highlight.dart';

import '../../core/theme.dart';
import 'neumorphic.dart';

/// Syntax colours tuned to the Graphite palette. The stock highlight themes
/// assume a much lighter or much darker background and read as noise here.
final Map<String, TextStyle> afHighlightTheme = {
  'root': const TextStyle(color: VonColors.textPrimary),
  'comment': const TextStyle(color: VonColors.textDisabled, fontStyle: FontStyle.italic),
  'quote': const TextStyle(color: VonColors.textDisabled),
  'keyword': const TextStyle(color: VonColors.accent, fontWeight: FontWeight.w600),
  'selector-tag': const TextStyle(color: VonColors.accent),
  'literal': const TextStyle(color: VonColors.accentText),
  'number': const TextStyle(color: Color(0xFFD8A0F0)),
  'string': const TextStyle(color: VonColors.success),
  'title': const TextStyle(color: VonColors.accentText),
  'function': const TextStyle(color: VonColors.accentText),
  'name': const TextStyle(color: VonColors.accentText),
  'attr': const TextStyle(color: VonColors.warn),
  'type': const TextStyle(color: VonColors.warn),
  'built_in': const TextStyle(color: VonColors.warn),
  'variable': const TextStyle(color: VonColors.textPrimary),
  'meta': const TextStyle(color: VonColors.textSecondary),
  'symbol': const TextStyle(color: VonColors.warn),
  'params': const TextStyle(color: VonColors.textPrimary),
  'deletion': const TextStyle(color: VonColors.danger),
  'addition': const TextStyle(color: VonColors.success),
};

/// Maps a fence tag to a highlight.js language id, guessing from the file
/// extension when the agent wrote something like ```report.py``` instead.
String? resolveLanguage(String tag) {
  final normalised = tag.trim().toLowerCase();
  final byExtension = normalised.contains('.')
      ? normalised.split('.').last
      : normalised;
  return switch (byExtension) {
    'py' || 'python' => 'python',
    'rs' || 'rust' => 'rust',
    'js' || 'javascript' || 'mjs' => 'javascript',
    'ts' || 'typescript' => 'typescript',
    'html' || 'htm' => 'xml',
    'css' => 'css',
    'json' => 'json',
    'yaml' || 'yml' => 'yaml',
    'sh' || 'bash' || 'shell' => 'bash',
    'md' || 'markdown' => 'markdown',
    'sql' => 'sql',
    'c' || 'h' => 'c',
    'cpp' || 'cc' || 'hpp' => 'cpp',
    'java' => 'java',
    'go' => 'go',
    'toml' => 'ini',
    'wat' => 'lisp',
    '' => null,
    _ => null,
  };
}

/// A code block inset into the bubble (Design.md §5.5: "Code blocks inside use
/// a darker bg_primary inset").
class CodeBlock extends StatelessWidget {
  const CodeBlock({super.key, required this.code, this.language, this.filename});

  final String code;
  final String? language;
  final String? filename;

  @override
  Widget build(BuildContext context) {
    final resolved = resolveLanguage(language ?? '');

    return Container(
      width: double.infinity,
      margin: const EdgeInsets.symmetric(vertical: VonSpace.sm),
      child: NeuSurface(
        depth: NeuDepth.concave,
        radius: VonRadius.small,
        intensity: 0.7,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(VonSpace.sm + 2, 6, 4, 0),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      filename ?? language ?? 'code',
                      style: VonText.caption.copyWith(fontSize: 10),
                    ),
                  ),
                  NeuIconButton(
                    icon: Icons.copy_rounded,
                    size: 26,
                    iconSize: 12,
                    tone: VonColors.textSecondary,
                    tooltip: 'Copy',
                    onPressed: () {
                      Clipboard.setData(ClipboardData(text: code));
                      ScaffoldMessenger.of(context).showSnackBar(
                        SnackBar(
                          duration: const Duration(seconds: 1),
                          backgroundColor: VonColors.bgElevated,
                          content: Text('Copied', style: VonText.body),
                        ),
                      );
                    },
                  ),
                ],
              ),
            ),
            // Code lines don't wrap: reflowed source is much harder to read
            // than source you scroll sideways through.
            SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.fromLTRB(
                VonSpace.sm + 2,
                0,
                VonSpace.sm + 2,
                VonSpace.sm,
              ),
              child: resolved == null
                  ? Text(code, style: VonText.code)
                  : HighlightView(
                      code,
                      language: resolved,
                      theme: afHighlightTheme,
                      padding: EdgeInsets.zero,
                      textStyle: VonText.code,
                    ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Raw program output — stdout, stderr, tracebacks. No highlighting: this is
/// a terminal, and colouring it would imply structure that isn't there.
class TerminalBlock extends StatelessWidget {
  const TerminalBlock({super.key, required this.text, this.maxLines = 18});

  final String text;
  final int maxLines;

  @override
  Widget build(BuildContext context) {
    final lines = text.trimRight().split('\n');
    final clipped = lines.length > maxLines;
    final shown = clipped ? lines.take(maxLines).join('\n') : text.trimRight();

    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.small,
      intensity: 0.6,
      padding: const EdgeInsets.all(VonSpace.sm),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: Text(shown, style: VonText.code.copyWith(fontSize: 11.5)),
          ),
          if (clipped)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                '… ${lines.length - maxLines} more lines',
                style: VonText.caption,
              ),
            ),
        ],
      ),
    );
  }
}

/// A deliberately small Markdown subset: fenced code, inline code, bold,
/// headings, and bullets.
///
/// A full Markdown engine would pull in a large dependency to render what the
/// agent actually emits, and would fight this theme's typography at every turn.
class MarkdownishText extends StatelessWidget {
  const MarkdownishText({super.key, required this.text, this.style});

  final String text;
  final TextStyle? style;

  @override
  Widget build(BuildContext context) {
    final base = style ?? VonText.body;
    final children = <Widget>[];
    final buffer = StringBuffer();

    void flushProse() {
      final prose = buffer.toString().trimRight();
      buffer.clear();
      if (prose.isEmpty) return;
      children.add(_ProseBlock(text: prose, style: base));
    }

    final lines = text.split('\n');
    var i = 0;
    while (i < lines.length) {
      final line = lines[i];

      if (line.trimLeft().startsWith('```')) {
        flushProse();
        final tag = line.trimLeft().substring(3).trim();
        final code = <String>[];
        i++;
        while (i < lines.length && !lines[i].trimLeft().startsWith('```')) {
          code.add(lines[i]);
          i++;
        }
        i++; // closing fence (or end of input on an unterminated block)
        children.add(
          CodeBlock(
            code: code.join('\n'),
            language: tag.isEmpty ? null : tag,
            filename: tag.contains('.') ? tag : null,
          ),
        );
        continue;
      }

      buffer.writeln(line);
      i++;
    }
    flushProse();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: children,
    );
  }
}

class _ProseBlock extends StatelessWidget {
  const _ProseBlock({required this.text, required this.style});

  final String text;
  final TextStyle style;

  @override
  Widget build(BuildContext context) {
    final blocks = <Widget>[];

    for (final line in text.split('\n')) {
      final trimmed = line.trimLeft();

      if (trimmed.startsWith('### ')) {
        blocks.add(_spaced(Text(trimmed.substring(4), style: VonText.heading)));
      } else if (trimmed.startsWith('## ')) {
        blocks.add(_spaced(Text(trimmed.substring(3), style: VonText.heading)));
      } else if (trimmed.startsWith('# ')) {
        blocks.add(_spaced(
          Text(trimmed.substring(2), style: VonText.title.copyWith(fontSize: 17)),
        ));
      } else if (trimmed.startsWith('- ') || trimmed.startsWith('* ')) {
        blocks.add(
          Padding(
            padding: const EdgeInsets.only(bottom: 3),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Padding(
                  padding: const EdgeInsets.only(top: 7, left: 2, right: 8),
                  child: Container(
                    width: 4,
                    height: 4,
                    decoration: const BoxDecoration(
                      shape: BoxShape.circle,
                      color: VonColors.accent,
                    ),
                  ),
                ),
                Expanded(child: _inline(trimmed.substring(2), style)),
              ],
            ),
          ),
        );
      } else if (trimmed.isEmpty) {
        blocks.add(const SizedBox(height: VonSpace.sm));
      } else {
        blocks.add(_inline(line, style));
      }
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: blocks,
    );
  }

  static Widget _spaced(Widget child) => Padding(
        padding: const EdgeInsets.only(top: VonSpace.sm, bottom: VonSpace.xs),
        child: child,
      );

  /// Handles `code` and **bold** within a line.
  static Widget _inline(String line, TextStyle style) {
    final spans = <TextSpan>[];
    final pattern = RegExp(r'`([^`]+)`|\*\*([^*]+)\*\*');
    var cursor = 0;

    for (final match in pattern.allMatches(line)) {
      if (match.start > cursor) {
        spans.add(TextSpan(text: line.substring(cursor, match.start)));
      }
      if (match.group(1) != null) {
        spans.add(TextSpan(
          text: match.group(1),
          style: VonText.code.copyWith(
            fontSize: style.fontSize! - 1,
            color: VonColors.accentText,
            backgroundColor: VonColors.bgPrimary,
          ),
        ));
      } else {
        spans.add(TextSpan(
          text: match.group(2),
          style: const TextStyle(fontWeight: FontWeight.w600),
        ));
      }
      cursor = match.end;
    }
    if (cursor < line.length) {
      spans.add(TextSpan(text: line.substring(cursor)));
    }

    return SelectableText.rich(TextSpan(style: style, children: spans));
  }
}
