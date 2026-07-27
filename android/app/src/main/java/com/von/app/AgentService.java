package com.von.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;

/**
 * Keeps VON's process alive while an agent run is in flight.
 *
 * <p>Everything the agent does runs on Rust threads inside this process. Android
 * does not know or care about those: once the activity is backgrounded the
 * process becomes a cached one, and a cached process holding a 1.7GB mmapped
 * model is the first thing the low-memory killer takes. From the user's side the
 * run simply vanished.
 *
 * <p>A foreground service is the only supported way to tell Android that work is
 * still happening. It does no work itself — it exists solely to raise the
 * process priority for as long as the run lasts, and it stops the moment the run
 * ends so VON is not sitting in the notification shade doing nothing.
 *
 * <p>Written in Java rather than Kotlin so the module needs no extra Gradle
 * plugin, and kept to two files because the alternative — a plugin package with
 * its own isolate model — is a great deal more machinery than "hold a wake
 * reference while this runs".
 */
public class AgentService extends Service {

    private static final String CHANNEL_ID = "von_agent_run";
    private static final int NOTIFICATION_ID = 4201;

    public static final String EXTRA_TITLE = "title";
    public static final String EXTRA_TEXT = "text";

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String title = "VON is working";
        String text = "The agent is running in this project.";
        if (intent != null) {
            if (intent.getStringExtra(EXTRA_TITLE) != null) {
                title = intent.getStringExtra(EXTRA_TITLE);
            }
            if (intent.getStringExtra(EXTRA_TEXT) != null) {
                text = intent.getStringExtra(EXTRA_TEXT);
            }
        }

        startForeground(NOTIFICATION_ID, buildNotification(title, text));

        // NOT_STICKY: if Android kills us anyway, the run is already gone with
        // the process. Restarting an empty service would put a notification on
        // screen for work that is not happening.
        return START_NOT_STICKY;
    }

    private Notification buildNotification(String title, String text) {
        NotificationManager manager = getSystemService(NotificationManager.class);

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel channel = new NotificationChannel(
                    CHANNEL_ID,
                    "Agent runs",
                    // LOW: no sound, no heads-up. The notification is a
                    // requirement of the platform, not something the user asked
                    // to be interrupted by.
                    NotificationManager.IMPORTANCE_LOW);
            channel.setDescription("Shown while VON is building something, so Android does not "
                    + "stop the work when you switch apps.");
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }

        // Tapping it returns to VON rather than launching a second copy.
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        PendingIntent tap = null;
        if (launch != null) {
            launch.setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP);
            int mutability = Build.VERSION.SDK_INT >= Build.VERSION_CODES.M
                    ? PendingIntent.FLAG_IMMUTABLE
                    : 0;
            tap = PendingIntent.getActivity(this, 0, launch, mutability);
        }

        Notification.Builder builder = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                ? new Notification.Builder(this, CHANNEL_ID)
                : new Notification.Builder(this);

        return builder
                .setContentTitle(title)
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setOngoing(true)
                .setContentIntent(tap)
                .build();
    }

    @Override
    public IBinder onBind(Intent intent) {
        // Nothing binds to this; it is started and stopped, not talked to.
        return null;
    }

    static void start(Context context, String title, String text) {
        Intent intent = new Intent(context, AgentService.class);
        intent.putExtra(EXTRA_TITLE, title);
        intent.putExtra(EXTRA_TEXT, text);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent);
        } else {
            context.startService(intent);
        }
    }

    static void stop(Context context) {
        context.stopService(new Intent(context, AgentService.class));
    }
}
