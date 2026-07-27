# Flutter's embedding is reached reflectively from the manifest; R8 cannot see
# those references and would otherwise strip FlutterActivity itself.
-keep class io.flutter.** { *; }
-keep class io.flutter.plugins.** { *; }
-dontwarn io.flutter.embedding.**

# webview_flutter's platform interface is instantiated by name.
-keep class io.flutter.plugins.webviewflutter.** { *; }

# flutter_rust_bridge resolves the native library and its symbols through JNI /
# dlopen. Nothing on the JVM side references them statically.
-keep class com.von.** { *; }

# Play Core is referenced by Flutter's deferred-components support, which this
# app does not use; without this R8 warns on the missing classes.
-dontwarn com.google.android.play.core.**
