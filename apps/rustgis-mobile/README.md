# rustgis-mobile

RustGIS on Android ([#1902](https://github.com/opengeos/RustGIS/issues/1902)). It runs the desktop
front end (`rustgis-ui-egui`) in a `NativeActivity`, with the system's file picker for opening and
saving, and the touch interface (`rustgis-ui-touch`) around its map, which can capture points,
lines and polygons with their attributes and show where the device is. It is not a field app yet:
a captured shape can't be edited by touch afterwards, positions aren't averaged or logged as a
track, and there are no photos.

## Build

Once:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --locked
# A JDK (for keytool and apksigner), and an Android SDK in ~/Android/Sdk (or ANDROID_HOME), with:
sdkmanager "ndk;27.2.12479018" "build-tools;36.0.0" "platforms;android-35" "platform-tools"
```

Then:

```sh
apps/rustgis-mobile/build-apk.sh             # arm64-v8a and x86_64, release
apps/rustgis-mobile/build-apk.sh x86_64      # the emulator's only
adb install -r target/android/rustgis-mobile.apk
adb shell am start -n org.rustgis.mobile/.MainActivity
adb logcat -s rustgis                        # the app's log
```

Open, Add Data and Save use Android's own file picker (see below). For testing, files can also be
pushed to the app's external files folder before starting the app: every file or folder there is
opened once the window is up, as if dropped on the desktop's window.

```sh
adb push parcels.gpkg /sdcard/Android/data/org.rustgis.mobile/files/
```

An emulator for testing without a phone:

```sh
sdkmanager "emulator" "system-images;android-35;default;x86_64"
avdmanager create avd -n rustgis-spike -k "system-images;android-35;default;x86_64" -d pixel_6
emulator -avd rustgis-spike -no-window -no-audio -gpu swiftshader_indirect
adb exec-out screencap -p > shot.png
```

The APK is signed with a debug key made in `target/android`. There is no Gradle project: `aapt2`
links the manifest, `javac` and `d8` compile the one Java class, and the Rust library is the rest
of the app.

## The phone, from Rust

`rustgis_engine::platform::Platform` is what a front end asks a phone for: files from the system's
picker, a place to save, and the device's position. The camera is added to it with its first
implementation. `src/phone.rs` implements it for Android by calling the activity's Java half,
`java/org/rustgis/mobile/MainActivity.java`, which is the only code that can show the picker (the
Storage Access Framework) and receive its answer.

Rust calls Java and never the other way: each frame the app collects what happened
(`takeEvents`), so the library exports nothing but `android_main`.

- **Opening.** A picked document is copied into the app's storage (`files/picked/…`), because the
  picker gives a document, not a path. Data and projects then open by path, so a GeoPackage is
  edited in place, in its copy: the original is not changed. Every other picker's file (a picture,
  a plugin, a delimited text file) goes to the dialog that asked, as in the browser.
- **Saving.** Save, Save As and the exports offer their file through the picker, as the browser
  offers a download. A project saved this way names its picked data by the copies' paths in the
  app's storage.
- **Not yet.** Choosing a folder (Add Folder, a Zarr store) says it isn't available. The copies of
  data and projects in `files/picked` are never removed (a dialog's file is removed once read). Bringing edits back to the original file is phase 3.

## The touch interface

A phone starts in the touch interface: the map, a bar of four large buttons under it (Layers, Add
Data, Capture, More) and sheets that rise over the map. The Layers sheet shows and hides layers,
zooms to one and makes one the active layer; the More sheet has Full Extent, the project, undo
and redo, and **Use the Desktop Interface**, which brings back the ribbon and panes until the app
is started again.

**Capture** adds a feature to one of the map's point, line or polygon layers. A cross marks the
middle of the map: the map is moved under it, and the sheet's fields are filled in (one for each
of the layer's attributes). For a point layer, **Add Point Here** puts the point at the cross.
For a line or polygon layer, **Add Vertex Here** places a vertex at the cross, the shape so far
is drawn over the map with a thin line on to the cross, **Undo Vertex** and **Start Over** take
vertices back, and **Finish Line** or **Finish Polygon** adds the feature once it has enough of
them (two for a line, three for a polygon). The sheet stays up, emptied, for the next feature.
Editing is turned on for the map when it was off, and the sheet says so.

The form follows the layer's data design. A field with a list of values (a coded-value domain,
the layer's subtypes, or yes and no) shows a chip for each: one is tapped to choose it, and
tapped again to leave the field empty. A field with a range says its limits. When the layer has
feature templates, a **Template** row chooses one, and its defaults fill what the form leaves
empty; the template stays chosen for the next feature. A field a constraint rule requires (the
rule asks `IS NOT NULL` of it, alone or joined to other clauses by `AND`) is marked with `*`, and a
feature with it empty, and no default for it from the template or subtype chosen, isn't added: the
sheet names the fields to fill in. A field in a contingent value set offers only the values that
fit what the set's other fields hold, as chips (a value chosen before the others changed stays,
so the conflict shows). A date or date and time field has a calendar button: a month of days a
finger tall each, with **Today** (in UTC; the phone's time zone isn't asked) and, for a date and
time, **Now**.

While a field is typed in, the sheet takes most of the window and stays above the soft keyboard:
the activity's Java half counts the keyboard in the window's bottom inset, the app looks again
for it while a field wants the keyboard (Android doesn't wake the app when it comes or goes), the
field is scrolled into view, and closing the keyboard ends the typing (Android 11 and later, which say when it goes; before, the field keeps the focus until the next tap, and the keyboard's room isn't counted).

**Show My Position**, in the More sheet, asks Android for the device's position (and the user,
the first time, for leave to use it). The position is drawn on the map as a dot in a ring as wide
as it is good to, and follows the device about once a second. **Go to My Position**, in the More
sheet and at the top of the Capture sheet, moves the map so the capture cross is on it: a point
or a vertex added then is at the device's position. **Hide My Position** stops asking, which
spares the battery.

What it doesn't do yet: moving or deleting a vertex once the feature is added, snapping, and a
new layer to capture into.

The interface keeps out of the room the system's bars take: the activity's Java half reports the
insets, and the bars' icons are drawn dark over a light theme's ground and light over a dark
one's.

## What differs from the desktop

| | On Android |
|---|---|
| File dialogs | Android's picker, through `Services::open_async` and `download` as in the browser. `rfd` has no Android backend, so what still asks it (choosing a folder) is cancelled and says so (`crates/ui-egui/src/filedialog.rs`) |
| HTTPS trust | The bundled Mozilla roots, not the device's certificates; the session says so once (`rustgis_data::http`) |
| Folders | Settings, app data and the cache are in the app's internal storage, set once at start-up (`rustgis_data::dirs::set_root`) |
| GeoLibre tools, GDAL formats, DuckDB | Not available: a phone can't run their programs. `rustgis_data::RUNS_PROGRAMS` is false on Android and iOS, so no program is looked for, and each place they would appear says so: a file read through GDAL or a DuckDB database is refused with the reason (convert it on a computer), the SQL pane says why it can't query, and the Geoprocessing pane shows a card in place of the GeoLibre tools. Add Data's Database… stays: its PostGIS, MySQL and SQL Server connections need no program. The app is built without the engine's `geolibre` feature |
| Preferences, the keychain, autosave | Not wired up yet |
| Soft keyboard | It opens for a text field, and the touch interface keeps the field above it. `NativeActivity`'s text input is still poor (no suggestions, no composing): a real app moves to `GameActivity` |

## What phase 0 found

On the Android 15 emulator (x86_64, software GPU), from this crate as it is:

| Checked | Result |
|---|---|
| Builds and links for `aarch64-linux-android` and `x86_64-linux-android` | Yes. The whole workspace below the apps compiles unchanged apart from `rfd`. The bundled SQLite and the Zstandard decoder build with the NDK's clang; the library needs only Android's own system libraries |
| Launches and draws the sample project | Yes, through wgpu on Vulkan, in under a second |
| Touch | Taps, menus and one-finger map panning work. Pinch zoom was not tested (the emulator can't be sent two fingers by `adb`) |
| Opens a GeoPackage | Yes |
| HTTPS basemap tiles | Yes, with the bundled roots |
| Rotation | Yes, without a restart |
| Suspend and resume | Yes: the surface is dropped and made again, the session is kept |
| The activity destroyed by the system | The session is lost. winit's event loop can't be made twice in a process, so the app ends its process and the next launch starts afresh; without that, the next launch showed nothing |
| Typing | Key events reach text fields. The soft keyboard itself needs a phone to judge |
| Size | 78 MB for arm64 without symbols, 89 MB in the APK for x86_64 |

What it showed is wrong or missing:

- (Fixed since.) The window drew under the status bar and the navigation bar.
- The desktop ribbon and panes are unusable at a phone's width, as expected. Phase 2 is a new front end.
- The map draws on the CPU here (the emulator has no GPU). Frame rates need a phone.
- Not tried on a phone: arm64, a real GPU, pinch zoom, the soft keyboard, memory limits.
