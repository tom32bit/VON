import 'package:von/core/theme.dart';
import 'package:von/ui/widgets/neumorphic.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// SWE §7.1 calls for widget tests over the two neomorphic states. Extruded vs
/// concave is the entire visual grammar — if a control renders in the wrong
/// state it reads as the wrong affordance, so these assert on the shadows
/// rather than on a golden image.

Future<void> _pump(WidgetTester tester, Widget child) {
  return tester.pumpWidget(
    MaterialApp(
      theme: buildVonTheme(),
      home: Scaffold(body: Center(child: child)),
    ),
  );
}

ShapeDecoration _decorationOf(WidgetTester tester) {
  final container = tester.widget<Container>(
    find
        .descendant(
          of: find.byType(NeuSurface),
          matching: find.byType(Container),
        )
        .first,
  );
  return container.decoration! as ShapeDecoration;
}

void main() {
  group('NeuSurface', () {
    testWidgets('extruded draws the light/dark shadow pair', (tester) async {
      await _pump(tester, const NeuSurface(child: SizedBox(width: 80, height: 40)));

      final shadows = _decorationOf(tester).shadows!;
      expect(shadows.length, 2);

      // Dark bottom-right, light top-left: the object is lifted off the surface.
      expect(shadows[0].color, VonColors.shadowDark);
      expect(shadows[0].offset, const Offset(4, 4));
      expect(shadows[1].color, VonColors.shadowLight);
      expect(shadows[1].offset, const Offset(-4, -4));
    });

    testWidgets('concave has no outer shadows and uses bg_primary',
        (tester) async {
      await _pump(
        tester,
        const NeuSurface(
          depth: NeuDepth.concave,
          child: SizedBox(width: 80, height: 40),
        ),
      );

      final decoration = _decorationOf(tester);
      // An outer shadow would contradict the inner-shadow painter.
      expect(decoration.shadows ?? const [], isEmpty);
      expect(decoration.color, VonColors.bgPrimary);
    });

    testWidgets('glow adds the accent halo on top of the shadow pair',
        (tester) async {
      await _pump(
        tester,
        const NeuSurface(glow: true, child: SizedBox(width: 80, height: 40)),
      );

      final shadows = _decorationOf(tester).shadows!;
      expect(shadows.length, 3);
      expect(shadows.last.color, VonColors.accentGlow);
    });

    testWidgets('intensity scales offset and blur together', (tester) async {
      await _pump(
        tester,
        const NeuSurface(intensity: 0.5, child: SizedBox(width: 40, height: 40)),
      );

      final shadows = _decorationOf(tester).shadows!;
      expect(shadows[0].offset, const Offset(2, 2));
      expect(shadows[0].blurRadius, 5);
    });

    testWidgets('is wrapped in a RepaintBoundary so shadows are cached',
        (tester) async {
      await _pump(tester, const NeuSurface(child: SizedBox(width: 40, height: 40)));

      expect(
        find.ancestor(
          of: find.byType(Container).first,
          matching: find.byType(RepaintBoundary),
        ),
        findsWidgets,
      );
    });
  });

  group('NeuPressable', () {
    testWidgets('inverts to concave while held and returns on release',
        (tester) async {
      await _pump(
        tester,
        NeuPressable(
          onTap: () {},
          child: const SizedBox(width: 100, height: 44),
        ),
      );

      expect(tester.widget<NeuSurface>(find.byType(NeuSurface)).depth,
          NeuDepth.extruded);

      final gesture = await tester.startGesture(
        tester.getCenter(find.byType(NeuPressable)),
      );
      await tester.pump(VonMotion.press);
      expect(tester.widget<NeuSurface>(find.byType(NeuSurface)).depth,
          NeuDepth.concave);

      await gesture.up();
      await tester.pumpAndSettle();
      expect(tester.widget<NeuSurface>(find.byType(NeuSurface)).depth,
          NeuDepth.extruded);
    });

    testWidgets('a selected control stays concave when not held',
        (tester) async {
      await _pump(
        tester,
        NeuPressable(
          onTap: () {},
          selected: true,
          child: const SizedBox(width: 100, height: 44),
        ),
      );

      expect(tester.widget<NeuSurface>(find.byType(NeuSurface)).depth,
          NeuDepth.concave);
    });

    testWidgets('a disabled control does not press or fire', (tester) async {
      var taps = 0;
      await _pump(
        tester,
        NeuPressable(
          onTap: () => taps++,
          enabled: false,
          child: const SizedBox(width: 100, height: 44),
        ),
      );

      await tester.tap(find.byType(NeuPressable));
      await tester.pumpAndSettle();

      expect(taps, 0);
      expect(tester.widget<NeuSurface>(find.byType(NeuSurface)).depth,
          NeuDepth.extruded);
    });
  });

  group('NeuPillTabs', () {
    testWidgets('exactly one tab is concave and it carries the accent',
        (tester) async {
      await _pump(
        tester,
        SizedBox(
          width: 360,
          child: NeuPillTabs(
            labels: const ['Chat', 'Files', 'Preview'],
            index: 1,
            onChanged: (_) {},
          ),
        ),
      );

      final depths = tester
          .widgetList<NeuSurface>(find.byType(NeuSurface))
          .map((s) => s.depth)
          .toList();
      expect(depths.where((d) => d == NeuDepth.concave).length, 1);

      final active = tester.widget<Text>(find.text('Files'));
      expect(active.style!.color, VonColors.accent);
      final inactive = tester.widget<Text>(find.text('Chat'));
      expect(inactive.style!.color, VonColors.textSecondary);
    });

    testWidgets('tapping reports the tapped index', (tester) async {
      int? tapped;
      await _pump(
        tester,
        SizedBox(
          width: 360,
          child: NeuPillTabs(
            labels: const ['Chat', 'Files'],
            index: 0,
            onChanged: (i) => tapped = i,
          ),
        ),
      );

      await tester.tap(find.text('Files'));
      expect(tapped, 1);
    });
  });

  group('NeuToggle', () {
    testWidgets('thumb turns accent and moves right when on', (tester) async {
      await _pump(tester, const NeuToggle(value: true));
      await tester.pumpAndSettle();

      final align = tester.widget<AnimatedAlign>(find.byType(AnimatedAlign));
      expect(align.alignment, Alignment.centerRight);

      final thumb = tester.widget<Container>(
        find
            .descendant(
              of: find.byType(AnimatedAlign),
              matching: find.byType(Container),
            )
            .last,
      );
      expect((thumb.decoration! as BoxDecoration).color, VonColors.accent);
    });

    testWidgets('off keeps the surface colour and sits left', (tester) async {
      await _pump(tester, const NeuToggle(value: false));
      await tester.pumpAndSettle();

      final align = tester.widget<AnimatedAlign>(find.byType(AnimatedAlign));
      expect(align.alignment, Alignment.centerLeft);
    });
  });

  group('NeuButton', () {
    testWidgets('disabled buttons show the disabled tone', (tester) async {
      await _pump(tester, const NeuButton(label: 'Run Agent'));

      expect(
        tester.widget<Text>(find.text('Run Agent')).style!.color,
        VonColors.textDisabled,
      );
    });

    testWidgets('enabled buttons use the accent', (tester) async {
      await _pump(tester, NeuButton(label: 'Run Agent', onPressed: () {}));

      expect(
        tester.widget<Text>(find.text('Run Agent')).style!.color,
        VonColors.accent,
      );
    });
  });
}
