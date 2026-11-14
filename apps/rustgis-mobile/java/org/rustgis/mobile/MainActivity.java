package org.rustgis.mobile;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;

/**
 * The activity: Android's NativeActivity, plus what only Java can ask the system for. The Rust
 * side (src/phone.rs) calls the public methods and collects what happened with takeEvents: it
 * is never called from here, so the library exports nothing but its entry point.
 *
 * An event is one string, its fields separated by tabs:
 *   picked    purpose  name  path    a chosen file, copied to the app's storage
 *   cancelled                        a picker closed without a choice
 *   saved     name                   a file written where the user chose
 *   position  lon  lat  accuracy     where the device is (accuracy in metres, or empty)
 *   stopped                          positioning ended without being asked to
 *   failed    text                   what went wrong, to show the user
 */
public class MainActivity extends NativeActivity {
    private static final int PICK = 1;
    private static final int SAVE = 2;
    private static final int LOCATION = 3;
    private static final int COPY_BUFFER = 1 << 16;

    private final ArrayList<String> events = new ArrayList<>();
    /** Pickers shown and not yet answered in full (guarded by events). */
    private int asked;
    /** The window's insets as insets() gives them, written on the main thread. */
    private volatile String insets = "";
    private String purpose = "";
    /** Whether positions are wanted (the main thread's alone). */
    private boolean locating;
    /** The listener positions arrive on, while they are asked for. */
    private android.location.LocationListener positions;
    /** The user is being asked for leave to use the location, and hasn't answered. */
    private boolean asking;
    /** The activity is in sight (between onStart and onStop). */
    private boolean started;
    /** A picker for opening is shown and has not answered (the main thread's alone). */
    private boolean picking;
    private String saveName = "";
    private byte[] saveBytes;

    @Override
    protected void onCreate(android.os.Bundle saved) {
        super.onCreate(saved);
        // The insets are read where Android hands them over, on the main thread, and kept for
        // the Rust side to ask for from its own.
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, given) -> {
            insets = describe(given);
            return view.onApplyWindowInsets(given);
        });
        barsLight(true);
    }

    /**
     * Draws the system bars' icons dark, for a light ground under them, or light, for a dark one:
     * the Rust side says which whenever its theme changes.
     */
    public void barsLight(final boolean light) {
        runOnUiThread(() -> {
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                android.view.WindowInsetsController bars = getWindow().getInsetsController();
                if (bars != null) {
                    int both = android.view.WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                            | android.view.WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                    bars.setSystemBarsAppearance(light ? both : 0, both);
                }
            } else {
                int both = android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                android.view.View view = getWindow().getDecorView();
                int flags = view.getSystemUiVisibility();
                view.setSystemUiVisibility(light ? flags | both : flags & ~both);
            }
        });
    }

    /**
     * Starts or stops reporting the device's position: each fix arrives as a "position" event
     * (longitude, latitude, accuracy in metres). Starting asks for the location permission where
     * it isn't given yet; a refusal arrives as a "failed" event.
     */
    public void locate(final boolean on) {
        runOnUiThread(() -> {
            locating = on;
            if (!on) {
                stopLocating();
            } else if (allowed(android.Manifest.permission.ACCESS_FINE_LOCATION)
                    || allowed(android.Manifest.permission.ACCESS_COARSE_LOCATION)) {
                startLocating();
            } else {
                asking = true;
                requestPermissions(new String[] {android.Manifest.permission.ACCESS_FINE_LOCATION,
                        android.Manifest.permission.ACCESS_COARSE_LOCATION}, LOCATION);
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request != LOCATION) {
            return;
        }
        asking = false;
        if (!locating) {
            return;
        }
        boolean given = false;
        for (int result : results) {
            given |= result == android.content.pm.PackageManager.PERMISSION_GRANTED;
        }
        if (given) {
            // Out of sight, the start is left to onStart: positions would only pile up.
            if (started) {
                startLocating();
            }
        } else {
            stopped("RustGIS wasn't given leave to use this device's location: its position can't be shown.");
        }
    }

    private boolean allowed(String permission) {
        return checkSelfPermission(permission) == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }

    /**
     * Positioning has ended without the Rust side asking: why is said ("failed"), and "stopped"
     * tells it so, so the interface doesn't go on offering to hide a position that isn't coming.
     */
    private void stopped(String why) {
        locating = false;
        stopLocating();
        add("failed\t" + why);
        add("stopped");
    }

    /** Positions are not asked for while the app is out of sight: they would only pile up. */
    @Override
    protected void onStop() {
        started = false;
        stopLocating();
        super.onStop();
    }

    /** Back in sight: positions are asked for again, if they were wanted and are still allowed. */
    @Override
    protected void onStart() {
        super.onStart();
        started = true;
        // While the user is still being asked for leave, its answer decides, not this.
        if (locating && !asking) {
            if (allowed(android.Manifest.permission.ACCESS_FINE_LOCATION)
                    || allowed(android.Manifest.permission.ACCESS_COARSE_LOCATION)) {
                startLocating();
            } else {
                stopped("RustGIS is no longer given leave to use this device's location.");
            }
        }
    }

    /** Asks every provider the device has (satellites, the network) for positions, about one a second. */
    private void startLocating() {
        android.location.LocationManager manager = (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
        if (manager == null) {
            stopped("This device has no location service.");
            return;
        }
        stopLocating();
        // Every callback is written out: before Android 11 none has a default, and a provider
        // switched off would call one that isn't there.
        positions = new android.location.LocationListener() {
            @Override
            public void onLocationChanged(android.location.Location fix) {
                add("position\t" + fix.getLongitude() + "\t" + fix.getLatitude() + "\t"
                        + (fix.hasAccuracy() ? Float.toString(fix.getAccuracy()) : ""));
            }

            @Override
            public void onProviderEnabled(String provider) {}

            /** Location switched off while positions are wanted: with no provider left, say so. */
            @Override
            public void onProviderDisabled(String provider) {
                android.location.LocationManager manager =
                        (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
                if (!locating || manager == null) {
                    return;
                }
                for (String other : new String[] {android.location.LocationManager.GPS_PROVIDER,
                        android.location.LocationManager.NETWORK_PROVIDER}) {
                    if (manager.getAllProviders().contains(other) && manager.isProviderEnabled(other)) {
                        return;
                    }
                }
                stopped("This device's location was turned off in its settings.");
            }

            @Override
            public void onStatusChanged(String provider, int status, android.os.Bundle extras) {}
        };
        int asked = 0;
        for (String provider : new String[] {android.location.LocationManager.GPS_PROVIDER,
                android.location.LocationManager.NETWORK_PROVIDER}) {
            try {
                // Satellites need the precise permission: with the approximate one alone, the
                // network's positions are the ones to ask for.
                boolean satellites = provider.equals(android.location.LocationManager.GPS_PROVIDER);
                // One switched off would be asked without complaint and never answer.
                if (manager.getAllProviders().contains(provider) && manager.isProviderEnabled(provider)
                        && (!satellites || allowed(android.Manifest.permission.ACCESS_FINE_LOCATION))) {
                    manager.requestLocationUpdates(provider, 1000, 0, positions, getMainLooper());
                    asked++;
                }
            } catch (SecurityException | IllegalArgumentException e) {
                add("failed\tThis device's location can't be read (" + provider + "): " + e.getMessage());
            }
        }
        if (asked == 0) {
            stopped("This device's position can't be found: location is turned off in its settings, or it has no way to find one.");
        }
    }

    private void stopLocating() {
        android.location.LocationManager manager = (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
        if (manager != null && positions != null) {
            manager.removeUpdates(positions);
        }
        positions = null;
    }

    @Override
    protected void onDestroy() {
        stopLocating();
        super.onDestroy();
    }

    /** Asks the user for files with the system's picker; each arrives as a "picked" event. */
    public void pick(final String purpose) {
        runOnUiThread(() -> {
            if (picking) {
                add("failed\tAnother file is already being chosen. Choose files again when it is done.");
                return;
            }
            picking = true;
            this.purpose = purpose;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            // Several files only where the front end takes several, as in the browser: two
            // projects would replace one another, and a dialog's options go with one file.
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, purpose.equals("data") || purpose.equals("shapefile"));
            if (!start(intent, PICK)) {
                picking = false;
            }
        });
    }

    /** Asks the user where to write a file, then writes it; a "saved" or "failed" event follows. */
    public void save(final String name, final byte[] bytes) {
        runOnUiThread(() -> {
            if (saveBytes != null) {
                add("failed\t" + name + " wasn't saved: another file is waiting for its place to be chosen.");
                return;
            }
            saveName = name;
            saveBytes = bytes;
            Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            // The file's own type where Android knows its extension: some providers name the
            // file by it.
            int dot = name.lastIndexOf('.');
            String type = dot < 0 ? null
                    : android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
            intent.setType(type == null ? "application/octet-stream" : type);
            intent.putExtra(Intent.EXTRA_TITLE, name);
            if (!start(intent, SAVE)) {
                saveBytes = null;
            }
        });
    }

    /** What happened since the last call, oldest first, one event to a line. */
    public String takeEvents() {
        synchronized (events) {
            String taken = String.join("\n", events);
            events.clear();
            return taken;
        }
    }

    /**
     * The room the system's bars and a camera cutout take at the window's edges, in pixels, as
     * "left top right bottom"; empty before the window is laid out.
     */
    public String insets() {
        return insets;
    }

    private static String describe(android.view.WindowInsets insets) {
        int left, top, right, bottom;
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets bars = insets.getInsets(
                    android.view.WindowInsets.Type.systemBars() | android.view.WindowInsets.Type.displayCutout());
            left = bars.left;
            top = bars.top;
            right = bars.right;
            bottom = bars.bottom;
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
        }
        return left + " " + top + " " + right + " " + bottom;
    }

    /** Whether something asked for is still to be reported. */
    public boolean busy() {
        synchronized (events) {
            return asked > 0 || !events.isEmpty();
        }
    }

    private boolean start(Intent intent, int request) {
        try {
            startActivityForResult(intent, request);
            synchronized (events) {
                asked++;
            }
            return true;
        } catch (RuntimeException e) {
            add("failed\tThis device has no file picker: " + e.getMessage());
            return false;
        }
    }

    private void add(String event) {
        synchronized (events) {
            events.add(event.replaceAll("[\\n\\r]", " "));
        }
    }

    /** One thing asked for (a picker shown) is answered in full. */
    private void answered() {
        synchronized (events) {
            asked--;
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == PICK) {
            picking = false;
            final ArrayList<Uri> uris = new ArrayList<>();
            if (result == RESULT_OK && data != null) {
                ClipData clip = data.getClipData();
                if (clip != null) {
                    for (int i = 0; i < clip.getItemCount(); i++) {
                        uris.add(clip.getItemAt(i).getUri());
                    }
                } else if (data.getData() != null) {
                    uris.add(data.getData());
                }
            }
            if (uris.isEmpty()) {
                add("cancelled");
                answered();
                return;
            }
            final String purpose = this.purpose;
            // Copied off the main thread: a document can be large, or come from the network.
            new Thread(() -> {
                // One folder for what was picked together, and for nothing else: a file that is
                // read with the one beside it (a .mif and its .mid) finds it, and a later pick of
                // the same name can't replace a file the session has open.
                File dir = new File(new File(getFilesDir(), "picked"), Long.toString(System.nanoTime()));
                try {
                    for (Uri uri : uris) {
                        copyIn(purpose, uri, dir);
                    }
                } finally {
                    answered();
                }
            }).start();
        } else if (request == SAVE) {
            final byte[] bytes = saveBytes;
            final String name = saveName;
            saveBytes = null;
            final Uri uri = result == RESULT_OK && data != null ? data.getData() : null;
            if (uri == null || bytes == null) {
                // Said, as the app has already told the user to choose a place for it.
                add("failed\t" + name + " wasn't saved: no place was chosen for it.");
                answered();
                return;
            }
            new Thread(() -> {
                try {
                    try (OutputStream out = getContentResolver().openOutputStream(uri, "wt")) {
                        if (out == null) {
                            throw new java.io.IOException("the place chosen can't be written to");
                        }
                        out.write(bytes);
                    }
                    // Said once the file is closed: a failure to close is a failure to save.
                    add("saved\t" + name);
                } catch (Throwable e) {
                    add("failed\t" + name + " wasn't saved: " + e.getMessage());
                } finally {
                    answered();
                }
            }).start();
        }
    }

    /** Copies a picked document into the app's storage, where the Rust side can open it by path. */
    private void copyIn(String purpose, Uri uri, File dir) {
        String name = displayName(uri);
        File file = new File(dir, name);
        // Two documents of one name picked together (from two folders) both keep their bytes.
        for (int n = 2; file.exists(); n++) {
            file = new File(dir, n + "-" + name);
        }
        try {
            if (!dir.isDirectory() && !dir.mkdirs()) {
                throw new java.io.IOException("the app's storage can't be written to");
            }
            try (InputStream in = getContentResolver().openInputStream(uri);
                    OutputStream out = new FileOutputStream(file)) {
                if (in == null) {
                    throw new java.io.IOException("it can't be read");
                }
                byte[] buffer = new byte[COPY_BUFFER];
                for (int n = in.read(buffer); n >= 0; n = in.read(buffer)) {
                    out.write(buffer, 0, n);
                }
            }
            // The copy's own name: a second document of one name is told from the first.
            add("picked\t" + purpose + "\t" + file.getName() + "\t" + file.getAbsolutePath());
        } catch (Throwable e) {
            // Half a copy would only fill the storage: nothing reports its path to remove it later.
            // (The folder goes only once it is empty.)
            file.delete();
            dir.delete();
            add("failed\t" + name + " wasn't opened: " + e.getMessage());
        }
    }

    /** A document's file name, without anything that would put it outside the folder it is copied to. */
    private String displayName(Uri uri) {
        String name = null;
        try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException e) {
            // The name is a convenience: the document is still read below.
        }
        if (name == null || name.isEmpty()) {
            name = "picked";
        }
        return name.replaceAll("[/\\\\\\t\\n\\r\\u0000]", "_").replaceAll("^\\.+", "_");
    }
}
