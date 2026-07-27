import 'package:von/core/theme.dart';
import 'package:von/ui/widgets/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// The agent's replies are rendered through a deliberately small Markdown
/// subset. These cover the cases models actually produce — including the
/// malformed ones, since a dropped closing fence must not swallow the answer.

Future<void> _pump(WidgetTester tester, String markdown) {
  return tester.pumpWidget(
    MaterialApp(
      theme: buildVonTheme(),
      home: Scaffold(
        body: SingleChildScrollView(child: MarkdownishText(text: markdown)),
      ),
    ),
  );
}

void main() {
  group('resolveLanguage', () {
    test('maps common extensions and aliases', () {
      expect(resolveLanguage('python'), 'python');
      expect(resolveLanguage('py'), 'python');
      expect(resolveLanguage('JS'), 'javascript');
      expect(resolveLanguage('yml'), 'yaml');
      // HTML highlights as XML in highlight.js.
      expect(resolveLanguage('html'), 'xml');
    });

    test('infers from a filename fence, which models often emit', () {
      expect(resolveLanguage('report.py'), 'python');
      expect(resolveLanguage('src/index.html'), 'xml');
    });

    test('returns null for empty or unknown tags rather than guessing', () {
      expect(resolveLanguage(''), isNull);
      expect(resolveLanguage('brainfuck'), isNull);
    });
  });

  group('MarkdownishText', () {
    testWidgets('renders a fenced block as a CodeBlock', (tester) async {
      await _pump(tester, 'Here you go:\n```python\nprint("hi")\n```\nDone.');

      expect(find.byType(CodeBlock), findsOneWidget);
      final block = tester.widget<CodeBlock>(find.byType(CodeBlock));
      expect(block.code, 'print("hi")');
      expect(block.language, 'python');
    });

    testWidgets('an unterminated fence still renders instead of vanishing',
        (tester) async {
      // Streaming means half-written fences are the normal case, not an edge one.
      await _pump(tester, 'Writing it now:\n```python\nprint("hi")');

      expect(find.byType(CodeBlock), findsOneWidget);
      expect(
        tester.widget<CodeBlock>(find.byType(CodeBlock)).code,
        'print("hi")',
      );
    });

    testWidgets('several blocks in one reply each get their own widget',
        (tester) async {
      await _pump(
        tester,
        '```python\na=1\n```\nand then\n```js\nlet b=2\n```',
      );

      expect(find.byType(CodeBlock), findsNWidgets(2));
    });

    testWidgets('a filename fence becomes the block header', (tester) async {
      await _pump(tester, '```src/main.py\nq = 0\n```');

      final block = tester.widget<CodeBlock>(find.byType(CodeBlock));
      expect(block.filename, 'src/main.py');
    });

    testWidgets('plain prose renders without a code block', (tester) async {
      await _pump(tester, 'I wrote the file and ran it. All 4 tests passed.');

      expect(find.byType(CodeBlock), findsNothing);
      expect(find.textContaining('All 4 tests passed'), findsOneWidget);
    });

    testWidgets('headings and bullets are structural, not literal text',
        (tester) async {
      await _pump(tester, '## Results\n- report.py works\n- chart.py works');

      // The marker characters must not survive into the rendered text.
      expect(find.text('Results'), findsOneWidget);
      expect(find.textContaining('## '), findsNothing);
      expect(find.textContaining('- report.py'), findsNothing);
    });
  });

  group('TerminalBlock', () {
    testWidgets('long output is clipped with a count of what was hidden',
        (tester) async {
      final output = List.generate(60, (i) => 'line $i').join('\n');
      await tester.pumpWidget(
        MaterialApp(
          theme: buildVonTheme(),
          home: Scaffold(body: TerminalBlock(text: output, maxLines: 10)),
        ),
      );

      expect(find.textContaining('50 more lines'), findsOneWidget);
    });

    testWidgets('short output is shown whole with no clip notice',
        (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(body: TerminalBlock(text: 'exit code 0\n42')),
        ),
      );

      expect(find.textContaining('more lines'), findsNothing);
      expect(find.textContaining('42'), findsOneWidget);
    });
  });
}
