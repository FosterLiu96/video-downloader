# Video Downloader

A small desktop app for downloading videos from X/Twitter, bilibili, brightcove and thousands of other sites. Built with [Tauri v2](https://tauri.app/) (Rust backend, vanilla HTML/JS frontend). Ships as a native installer for macOS and Windows. *Will not work with Youtube*

[![Release](https://img.shields.io/github/v/release/FosterLiu96/video-downloader?style=for-the-badge)](https://github.com/FosterLiu96/video-downloader/releases)
[![Downloads](https://img.shields.io/github/downloads/FosterLiu96/video-downloader/total?style=for-the-badge)](https://github.com/FosterLiu96/video-downloader/releases)
[![License](https://img.shields.io/github/license/FosterLiu96/video-downloader?style=for-the-badge)](LICENSE)

On first launch the app downloads `yt-dlp` and `ffmpeg` automatically — no manual setup needed.

---

## Features

- Paste any URL and pick a quality (Best, 1080p, 720p, 480p, Audio Only, MP3)
- Merges video + audio streams into a single `.mp4` via ffmpeg
- Cookie passthrough from Safari, Chrome, Firefox, Edge, or Brave for members-only or age-gated content
- Live download log and progress bar
- Cancel stops the download immediately (kills yt-dlp and any spawned ffmpeg child processes)

---

## Project structure

```
├── src/
│   ├── index.html      # Single-page UI (setup screen + main screen)
│   ├── main.js         # Frontend logic — Tauri event listeners, UI interactions
│   └── style.css       # Styles
├── src-tauri/
│   ├── src/lib.rs      # Rust backend — commands: start_download, cancel_download,
│   │                   #   download_deps, check_deps, open_folder
│   ├── Cargo.toml
│   └── tauri.conf.json
├── assets/
│   └── dmg-background.jpg
└── .github/workflows/build.yml   # CI: builds installers for macOS + Windows
```

**Runtime dependencies** (downloaded automatically into the app data directory on first launch):

| Tool | Source |
|------|--------|
| `yt-dlp` | GitHub releases — `yt-dlp_macos` / `yt-dlp.exe` |
| `ffmpeg` | evermeet.cx (macOS) / BtbN ffmpeg-builds (Windows) |

**Rust crate dependencies** (see `src-tauri/Cargo.toml`): `tauri 2`, `tokio`, `reqwest`, `zip`, `dirs`, `futures-util`.

---

## Building from source

### Prerequisites

- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/) stable
- **macOS:** Xcode Command Line Tools (`xcode-select --install`)
- **Windows:** [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) or Visual Studio with the "Desktop development with C++" workload. WebView2 is required (pre-installed on Windows 11; downloadable for Windows 10).

```bash
npm install
npm run build        # produces installer in src-tauri/target/release/bundle/
```

For development with hot-reload:

```bash
npm run dev
```

### Releases

Pushing a tag of the form `v1.2.3` triggers the GitHub Actions workflow and produces:

- **macOS:** `.app` bundle + DMG
- **Windows:** NSIS `.exe` installer + `.msi`

---

## Caveats

### Windows — Chrome cookie database is locked

Chrome holds an exclusive lock on its cookie database while running. If you select **Chrome** cookies and see an error about a locked database, either:

- Close Chrome completely before downloading, or
- Switch to **Firefox** (or another browser) in the cookie selector.

### macOS — Safari cookies require Full Disk Access

yt-dlp reads Safari's cookie store from `~/Library/Cookies/Cookies.binarycookies`. If it fails silently, grant **Full Disk Access** to the app in *System Settings → Privacy & Security → Full Disk Access*.

### Age-gated or members-only videos

Select the browser you use for the site in the **"Use cookies from browser"** dropdown. The app passes your login cookies to yt-dlp so it can authenticate. Make sure you are logged in to the site in that browser before downloading.

### First-launch download

On the first run the app fetches `yt-dlp` (~10 MB) and `ffmpeg` (~70–90 MB). This happens once; subsequent launches skip it. If the download fails (e.g. no internet connection), restart the app to retry.

### Antivirus false positives (Windows)

Some antivirus products flag unsigned Tauri binaries or bundled yt-dlp/ffmpeg as suspicious. If the installer or the downloaded tools are quarantined, add an exception for the app data directory (`%APPDATA%\com.internal.videodownloader\bin`).
