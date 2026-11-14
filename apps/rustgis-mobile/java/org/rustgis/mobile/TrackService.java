package org.rustgis.mobile;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;

/**
 * Keeps the app's positions coming while it is out of sight, for a track being recorded: a
 * foreground service of the location type, with the notification Android requires for one. It
 * does no work of its own. While it runs, the activity's location listener (MainActivity) goes on
 * receiving positions after the activity stops, and they wait in its events for the Rust side,
 * which takes them all when the app is back in sight.
 *
 * The activity starts it while it is in sight (Android refuses a location service started from the
 * background) and stops it when the track ends.
 */
public class TrackService extends Service {
    private static final String CHANNEL = "track";
    private static final int NOTICE = 1;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager notices = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (notices != null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, "Track recording", NotificationManager.IMPORTANCE_LOW);
            channel.setDescription("Shown while RustGIS records a track out of sight.");
            notices.createNotificationChannel(channel);
        }
        // Tapping the notice brings the app back.
        Intent open = new Intent(this, MainActivity.class).setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent back = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification notice = new Notification.Builder(this, CHANNEL)
                .setContentTitle("RustGIS is recording a track")
                .setContentText("Positions are kept while the app is out of sight. Open RustGIS to stop.")
                .setSmallIcon(android.R.drawable.ic_menu_mylocation)
                .setContentIntent(back)
                .setOngoing(true)
                .build();
        try {
            if (android.os.Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTICE, notice, android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION);
            } else {
                startForeground(NOTICE, notice);
            }
        } catch (RuntimeException e) {
            // Refused (no location permission, or started out of sight): the activity said it
            // couldn't keep positions coming, from the reason it reads back.
            MainActivity.serviceRefused("Positions can't be kept while RustGIS is out of sight: " + e.getMessage());
            stopSelf();
            return START_NOT_STICKY;
        }
        // Not restarted by Android if it ends the process: the track it served is gone with it.
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
