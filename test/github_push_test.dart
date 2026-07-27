import 'package:flutter_test/flutter_test.dart';
import 'package:von/ui/sheets/github_push_sheet.dart';

void main() {
  group('repoSlug', () {
    test('lowercases and hyphenates a plain name', () {
      expect(repoSlug('Report Generator', 3), 'report-generator');
    });

    test('keeps the characters GitHub actually allows', () {
      expect(repoSlug('von_2.0-beta', 1), 'von_2.0-beta');
    });

    test('collapses runs and trims the edges', () {
      // A leading hyphen or a trailing dot is rejected by GitHub, so a name
      // that produces one has to be cleaned rather than merely filtered.
      expect(repoSlug('  My   Cool!!! Project  ', 1), 'my-cool-project');
      expect(repoSlug('...leading dots', 1), 'leading-dots');
    });

    test('falls back to the ordinal when nothing survives', () {
      // A suggestion the user has to delete before typing is worse than a dull
      // one, so this never returns empty.
      expect(repoSlug('!!!', 7), 'von-7');
      expect(repoSlug('', 12), 'von-12');
      expect(repoSlug('日本語', 4), 'von-4');
    });
  });
}
