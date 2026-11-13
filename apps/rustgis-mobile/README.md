# rustgis-mobile

RustGIS on Android: the phase 0 feasibility spike of
[#1902](https://github.com/opengeos/RustGIS/issues/1902). It runs the desktop front end
(`rustgis-ui-egui`) in a `NativeActivity` to find out what builds, what runs and what breaks on a
phone. It is not a field app yet: there is no touch interface, no location and no file picker.

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
adb shell am start -n org.rustgis.mobile/android.app.NativeActivity
adb logcat -s rustgis                        # the app's log
```

There is no file picker yet. To open data, push it to the app's external files folder before
starting the app: every file or folder there is opened once the window is up, as if dropped on the desktop's
window.

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

The APK is signed with a debug key made in `target/android`. There is no Gradle project and no
Java or Kotlin: `aapt2` links the manifest, and the Rust library is the whole app.

## What differs from the desktop

| | On Android |
|---|---|
| File dialogs | None: `rfd` has no Android backend, so every picker is cancelled (`crates/ui-egui/src/filedialog.rs`); the app says so once at start-up, and each cancelled picker is in the log |
| HTTPS trust | The bundled Mozilla roots, not the device's certificates; the session says so once (`rustgis_data::http`) |
| Folders | Settings, app data and the cache are in the app's internal storage, set once at start-up (`rustgis_data::dirs::set_root`) |
| GeoLibre tools, GDAL formats, DuckDB | Not available: a phone can't run their programs |
| Preferences, the keychain, autosave | Not wired up yet |
| Soft keyboard | `NativeActivity`'s is poor; a real app moves to `GameActivity` |

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
