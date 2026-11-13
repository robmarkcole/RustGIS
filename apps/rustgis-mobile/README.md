# rustgis-mobile

RustGIS on Android ([#1902](https://github.com/opengeos/RustGIS/issues/1902)). It runs the desktop
front end (`rustgis-ui-egui`) in a `NativeActivity`, with the system's file picker for opening and
saving. It is not a field app yet: there is no touch interface and no location.

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
picker and a place to save. Location and the camera are added to it with their
first implementation. `src/phone.rs` implements it for Android by calling the activity's Java half,
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

## What differs from the desktop

| | On Android |
|---|---|
| File dialogs | Android's picker, through `Services::open_async` and `download` as in the browser. `rfd` has no Android backend, so what still asks it (choosing a folder) is cancelled and says so (`crates/ui-egui/src/filedialog.rs`) |
| HTTPS trust | The bundled Mozilla roots, not the device's certificates; the session says so once (`rustgis_data::http`) |
| Folders | Settings, app data and the cache are in the app's internal storage, set once at start-up (`rustgis_data::dirs::set_root`) |
| GeoLibre tools, GDAL formats, DuckDB | Not available: a phone can't run their programs |
| Preferences, the keychain, autosave | Not wired up yet |
| Soft keyboard | It opens for a text field. `NativeActivity`'s is poor; a real app moves to `GameActivity` |

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

- The window draws under the status bar and the navigation bar: the app ignores the system's insets.
- The desktop ribbon and panes are unusable at a phone's width, as expected. Phase 2 is a new front end.
- The map draws on the CPU here (the emulator has no GPU). Frame rates need a phone.
- Not tried on a phone: arm64, a real GPU, pinch zoom, the soft keyboard, memory limits.
