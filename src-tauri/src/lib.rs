use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex};

// ── Shared state ──────────────────────────────────────────────────────────────

pub struct AppState {
    cancel_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            cancel_tx: Mutex::new(None),
        }
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn bin_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap().join("bin")
}

fn ytdlp_path(app: &AppHandle) -> PathBuf {
    bin_dir(app).join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" })
}

fn ffmpeg_path(app: &AppHandle) -> PathBuf {
    bin_dir(app).join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" })
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn check_deps(app: AppHandle) -> bool {
    ytdlp_path(&app).exists() && ffmpeg_path(&app).exists()
}

#[tauri::command]
fn get_default_output_path() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DownloadedVideos")
        .to_string_lossy()
        .into_owned()
}

#[tauri::command]
async fn download_deps(app: AppHandle) -> Result<(), String> {
    let dir = bin_dir(&app);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // — yt-dlp —
    if !ytdlp_path(&app).exists() {
        let url = if cfg!(windows) {
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
        } else {
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos"
        };
        app.emit("setup-task", "Downloading yt-dlp…").ok();
        download_file(&app, url, &ytdlp_path(&app), 0.0, 0.12).await?;
        make_executable(&ytdlp_path(&app));
    }

    // — ffmpeg —
    if !ffmpeg_path(&app).exists() {
        let (ffmpeg_url, ffmpeg_bin) = if cfg!(windows) {
            (
                "https://github.com/BtbN/ffmpeg-builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip",
                "ffmpeg.exe",
            )
        } else {
            ("https://evermeet.cx/ffmpeg/getrelease/zip", "ffmpeg")
        };

        app.emit("setup-task", "Downloading ffmpeg…").ok();
        let zip_path = dir.join("ffmpeg.zip");
        download_file(&app, ffmpeg_url, &zip_path, 0.12, 0.93).await?;

        app.emit("setup-task", "Extracting ffmpeg…").ok();
        app.emit("setup-progress", 0.93_f64).ok();

        let dest = ffmpeg_path(&app);
        tokio::task::spawn_blocking(move || {
            extract_binary(&zip_path, ffmpeg_bin, &dest)?;
            std::fs::remove_file(&zip_path).ok();
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())??;

        make_executable(&ffmpeg_path(&app));
    }

    app.emit("setup-progress", 1.0_f64).ok();
    app.emit("setup-done", ()).ok();
    Ok(())
}

#[tauri::command]
async fn start_download(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    format_args: Vec<String>,
    output_path: String,
    cookie_browser: String,
) -> Result<(), String> {
    use tokio::io::AsyncBufReadExt;

    // Cancel any running download first
    {
        let mut lock = state.cancel_tx.lock().await;
        if let Some(tx) = lock.take() {
            let _ = tx.send(());
        }
    }

    // Build yt-dlp argument list
    let mut args: Vec<String> = format_args;
    if cookie_browser != "none" && !cookie_browser.is_empty() {
        args.push("--cookies-from-browser".to_string());
        args.push(cookie_browser);
    }
    let ffmpeg_dir = bin_dir(&app).to_string_lossy().into_owned();
    args.extend([
        "-S".to_string(),
        "vcodec:h264,res,fps,br".to_string(),
        "--merge-output-format".to_string(),
        "mp4".to_string(),
        "--ffmpeg-location".to_string(),
        ffmpeg_dir,
        "--newline".to_string(),
        "-P".to_string(),
        output_path.clone(),
        url,
    ]);

    std::fs::create_dir_all(&output_path)
        .map_err(|e| format!("Cannot create output folder: {}", e))?;

    let mut child = tokio::process::Command::new(ytdlp_path(&app))
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch yt-dlp: {}", e))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    *state.cancel_tx.lock().await = Some(cancel_tx);

    // Stream stdout → frontend (with progress parsing)
    let app1 = app.clone();
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            app1.emit("download-log", &line).ok();
            if let Some(pct) = parse_progress(&line) {
                app1.emit("download-progress", pct / 100.0).ok();
            }
        }
    });

    // Stream stderr → frontend
    let app2 = app.clone();
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            app2.emit("download-log", &line).ok();
        }
    });

    // Wait for exit or cancellation
    let app3 = app.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_c = cancelled.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                if cancelled.load(Ordering::SeqCst) { return; }
                match result {
                    Ok(status) => {
                        if status.code() == Some(0) {
                            app3.emit("download-complete", ()).ok();
                        } else {
                            let code = status.code().unwrap_or(-1);
                            app3.emit("download-error",
                                format!("yt-dlp exited with code {}", code)).ok();
                        }
                    }
                    Err(e) => { app3.emit("download-error", e.to_string()).ok(); }
                }
            }
            _ = cancel_rx => {
                cancelled_c.store(true, Ordering::SeqCst);
                child.kill().await.ok();
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn cancel_download(state: State<'_, AppState>) -> Result<(), ()> {
    let mut lock = state.cancel_tx.lock().await;
    if let Some(tx) = lock.take() {
        let _ = tx.send(());
    }
    Ok(())
}

#[tauri::command]
fn open_folder(path: String) {
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(&path).spawn().ok(); }
    #[cfg(target_os = "windows")]
    { std::process::Command::new("explorer").arg(&path).spawn().ok(); }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&path).spawn().ok(); }
}

// ── Private helpers ───────────────────────────────────────────────────────────

async fn download_file(
    app: &AppHandle,
    url: &str,
    dest: &std::path::Path,
    start: f64,
    end: f64,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    let total = resp.content_length().unwrap_or(0);
    let mut received: u64 = 0;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        if total > 0 {
            let frac = received as f64 / total as f64;
            let p = start + frac * (end - start);
            app.emit("setup-progress", p).ok();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn extract_binary(
    zip_path: &std::path::Path,
    binary_name: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        let fname = std::path::Path::new(&name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if fname == binary_name {
            let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Err(format!("{} not found in archive", binary_name))
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).ok();
    }
}
#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}

fn parse_progress(line: &str) -> Option<f64> {
    let pct_idx = line.find('%')?;
    let before = &line[..pct_idx];
    let num_start = before
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map(|i| i + 1)
        .unwrap_or(0);
    before[num_start..].parse::<f64>().ok()
}

// ── App entry point ───────────────────────────────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            check_deps,
            download_deps,
            get_default_output_path,
            start_download,
            cancel_download,
            open_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
