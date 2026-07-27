import '../services/core_api.dart';

/// Turns anything thrown across the bridge into text worth showing a user.
///
/// `flutter_rust_bridge` generates `VonException` as a plain Dart class with no
/// `toString` override, so `'$e'` renders the literal string
/// `Instance of 'VonException'` — which is how a Gemini key failure reached the
/// screen carrying none of the reason it failed. The fields are right there;
/// this just reads them.
String describeError(Object? error) {
  if (error == null) return 'Something went wrong.';
  if (error is VonException) {
    final message = error.message.trim();
    return message.isEmpty ? 'Something went wrong (${error.kind}).' : message;
  }

  final text = error.toString().trim();
  if (text.isEmpty || text.startsWith('Instance of')) {
    return 'Something went wrong.';
  }

  // Anything that does print its fields — an older generated shape, or a
  // wrapped error — still has the useful half buried in the middle.
  final fields = RegExp(r'message:\s*(.+?)(?:,\s*transient|\))').firstMatch(text);
  if (fields != null) return fields.group(1)!.trim();

  // Dart prefixes uncaught errors with "Exception: ", which tells nobody
  // anything.
  return text.replaceFirst(RegExp(r'^(Exception|Error):\s*'), '');
}

/// Whether retrying the identical call could plausibly work.
///
/// Drives whether an error banner offers a Retry at all: a button that will
/// certainly fail again is worse than no button.
bool isTransient(Object? error) =>
    error is VonException ? error.transient : false;
