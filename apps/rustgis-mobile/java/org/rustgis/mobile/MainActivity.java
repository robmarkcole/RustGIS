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
 *   failed    text                   what went wrong, to show the user
 */
public class MainActivity extends NativeActivity {
    private static final int PICK = 1;
    private static final int SAVE = 2;
    private static final int COPY_BUFFER = 1 << 16;

    private final ArrayList<String> events = new ArrayList<>();
    /** Pickers shown and not yet answered in full (guarded by events). */
    private int asked;
    private String purpose = "";
    /** A picker for opening is shown and has not answered (the main thread's alone). */
    private boolean picking;
    private String saveName = "";
    private byte[] saveBytes;

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
