package com.von.app;

import androidx.annotation.NonNull;

import io.flutter.embedding.android.FlutterActivity;
import io.flutter.embedding.engine.FlutterEngine;
import io.flutter.plugin.common.MethodChannel;

/**
 * The app's only activity.
 *
 * <p>VON previously pointed the manifest straight at {@link FlutterActivity} and
 * shipped no JVM source at all, which was a nice property right up until it
 * wasn't: keeping a backgrounded run alive needs a foreground service, and
 * starting one needs a context and a method channel. Two small Java files is the
 * whole cost.
 *
 * <p>The channel is deliberately thin. It starts and stops a service; it does
 * not marshal state, and Dart remains the only thing that decides when a run is
 * happening.
 */
public class MainActivity extends FlutterActivity {

    private static final String CHANNEL = "com.von.app/agent_service";

    @Override
    public void configureFlutterEngine(@NonNull FlutterEngine engine) {
        super.configureFlutterEngine(engine);

        new MethodChannel(engine.getDartExecutor().getBinaryMessenger(), CHANNEL)
                .setMethodCallHandler((call, result) -> {
                    switch (call.method) {
                        case "start":
                            String title = call.argument("title");
                            String text = call.argument("text");
                            AgentService.start(
                                    getApplicationContext(),
                                    title != null ? title : "VON is working",
                                    text != null ? text : "The agent is running.");
                            result.success(null);
                            break;
                        case "stop":
                            AgentService.stop(getApplicationContext());
                            result.success(null);
                            break;
                        default:
                            result.notImplemented();
                    }
                });
    }
}
