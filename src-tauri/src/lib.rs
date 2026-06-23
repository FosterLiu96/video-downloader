use serde::{Deserialize, Serialize};
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
    cookie_dir: PathBuf,
}

impl Default for AppState {
    fn default() -> Self {
        cleanup_stale_cookie_dirs();
        let cookie_dir =
            std::env::temp_dir().join(format!("video-downloader-cookies-{}", std::process::id()));
        std::fs::remove_dir_all(&cookie_dir).ok();
        std::fs::create_dir_all(&cookie_dir).ok();
        secure_cookie_dir(&cookie_dir);

        Self {
            cancel_tx: Mutex::new(None),
            cookie_dir,
        }
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.cookie_dir).ok();
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn bin_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap().join("bin")
}

fn ytdlp_path(app: &AppHandle) -> PathBuf {
    bin_dir(app).join(if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    })
}

fn ffmpeg_path(app: &AppHandle) -> PathBuf {
    bin_dir(app).join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    })
}

fn ytdlp_download_url() -> &'static str {
    if cfg!(windows) {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    } else {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos"
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
async fn check_deps(app: AppHandle) -> bool {
    read_ytdlp_version(&ytdlp_path(&app)).await.is_ok()
        && ffmpeg_is_working(&ffmpeg_path(&app)).await
}

#[tauri::command]
async fn get_ytdlp_version(app: AppHandle) -> Result<String, String> {
    read_ytdlp_version(&ytdlp_path(&app)).await
}

#[derive(Serialize)]
struct YtdlpStatus {
    current_version: String,
    latest_version: Option<String>,
    update_available: bool,
    days_outdated: Option<i64>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

#[tauri::command]
async fn get_ytdlp_status(app: AppHandle) -> Result<YtdlpStatus, String> {
    let current_version = read_ytdlp_version(&ytdlp_path(&app)).await?;
    let latest_version = fetch_latest_ytdlp_version().await.ok();
    let update_available = latest_version
        .as_ref()
        .map(
            |latest| match (version_day(&current_version), version_day(latest)) {
                (Some(current_day), Some(latest_day)) => latest_day > current_day,
                _ => latest != &current_version,
            },
        )
        .unwrap_or(false);
    let days_outdated = latest_version.as_ref().and_then(|latest| {
        let current_day = version_day(&current_version)?;
        let latest_day = version_day(latest)?;
        (latest_day > current_day).then_some(latest_day - current_day)
    });

    Ok(YtdlpStatus {
        current_version,
        latest_version,
        update_available,
        days_outdated,
    })
}

#[derive(Serialize)]
struct YtdlpUpdateResult {
    previous_version: String,
    current_version: String,
    updated: bool,
}

#[tauri::command]
async fn update_ytdlp(app: AppHandle) -> Result<YtdlpUpdateResult, String> {
    let current_path = ytdlp_path(&app);
    let previous_version = read_ytdlp_version(&current_path)
        .await
        .unwrap_or_else(|_| "Unknown".to_string());
    let update_path = bin_dir(&app).join(if cfg!(windows) {
        "yt-dlp-update.exe"
    } else {
        "yt-dlp-update"
    });

    if update_path.exists() {
        tokio::fs::remove_file(&update_path)
            .await
            .map_err(|e| format!("Cannot remove old update file: {e}"))?;
    }

    if let Err(error) = download_to_path(ytdlp_download_url(), &update_path).await {
        tokio::fs::remove_file(&update_path).await.ok();
        return Err(error);
    }
    make_executable(&update_path);

    let current_version = match read_ytdlp_version(&update_path).await {
        Ok(version) => version,
        Err(error) => {
            tokio::fs::remove_file(&update_path).await.ok();
            return Err(format!("Downloaded yt-dlp could not be validated: {error}"));
        }
    };

    if current_version == previous_version {
        tokio::fs::remove_file(&update_path).await.ok();
        return Ok(YtdlpUpdateResult {
            previous_version,
            current_version,
            updated: false,
        });
    }

    replace_ytdlp(&current_path, &update_path).await?;

    Ok(YtdlpUpdateResult {
        previous_version,
        current_version,
        updated: true,
    })
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
    if read_ytdlp_version(&ytdlp_path(&app)).await.is_err() {
        tokio::fs::remove_file(ytdlp_path(&app)).await.ok();
        app.emit("setup-task", "Downloading yt-dlp…").ok();
        download_file(&app, ytdlp_download_url(), &ytdlp_path(&app), 0.0, 0.12).await?;
        make_executable(&ytdlp_path(&app));
        read_ytdlp_version(&ytdlp_path(&app))
            .await
            .map_err(|e| format!("Downloaded yt-dlp could not be validated: {e}"))?;
    }

    // — ffmpeg —
    if !ffmpeg_is_working(&ffmpeg_path(&app)).await {
        tokio::fs::remove_file(ffmpeg_path(&app)).await.ok();
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
        if !ffmpeg_is_working(&ffmpeg_path(&app)).await {
            tokio::fs::remove_file(ffmpeg_path(&app)).await.ok();
            return Err("Downloaded ffmpeg could not be validated".to_string());
        }
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
    args.push("--no-ignore-errors".to_string());
    let session_cookie_path = cookie_path_for_browser(&state.cookie_dir, &cookie_browser)?;
    if let Some(cookie_path) = &session_cookie_path {
        let has_cached_cookies = cookie_jar_has_entries(cookie_path);
        let cookie_path = cookie_path.to_string_lossy().into_owned();
        if has_cached_cookies {
            args.push("--cookies".to_string());
            args.push(cookie_path);
        } else {
            args.push("--cookies-from-browser".to_string());
            args.push(cookie_browser);
            args.push("--cookies".to_string());
            args.push(cookie_path);
        }
    }
    let ffmpeg_dir = bin_dir(&app).to_string_lossy().into_owned();
    args.extend([
        "-S".to_string(),
        "res,fps,br".to_string(),
        "--merge-output-format".to_string(),
        "mp4".to_string(),
        "--remux-video".to_string(),
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

    let mut cmd = tokio::process::Command::new(ytdlp_path(&app));
    cmd.args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Put yt-dlp in its own process group so that killing it also kills
    // any child processes it spawns (e.g. ffmpeg for merging).
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to launch yt-dlp: {}", e))?;
    let sleep_guard = prevent_sleep(child.id());

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
        let _sleep_guard = sleep_guard;
        tokio::select! {
            result = child.wait() => {
                if !cancelled.load(Ordering::SeqCst) {
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
            }
            _ = cancel_rx => {
                cancelled_c.store(true, Ordering::SeqCst);
                // Kill the whole process group so ffmpeg (spawned by yt-dlp)
                // is also terminated.
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    std::process::Command::new("kill")
                        .args(["-KILL", &format!("-{}", pid)])
                        .status()
                        .ok();
                }
                #[cfg(windows)]
                if let Some(pid) = child.id() {
                    std::process::Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .status()
                        .ok();
                }
                child.kill().await.ok();
            }
        }
        if let Some(cookie_path) = session_cookie_path {
            secure_cookie_file(&cookie_path);
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
    {
        std::process::Command::new("open").arg(&path).spawn().ok();
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .ok();
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn cookie_path_for_browser(
    cookie_dir: &std::path::Path,
    browser: &str,
) -> Result<Option<PathBuf>, String> {
    match browser {
        "" | "none" => Ok(None),
        "safari" | "chrome" | "firefox" | "edge" | "brave" => {
            Ok(Some(cookie_dir.join(format!("{browser}.txt"))))
        }
        _ => Err("Unsupported cookie browser".to_string()),
    }
}

fn cleanup_stale_cookie_dirs() {
    let temp_dir = std::env::temp_dir();
    let current_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir(&temp_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid) = name
            .strip_prefix("video-downloader-cookies-")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };

        if pid != current_pid && !process_is_running(pid) {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn process_is_running(_pid: u32) -> bool {
    false
}

struct SleepGuard {
    helper: Option<std::process::Child>,
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.as_mut() {
            helper.kill().ok();
            helper.wait().ok();
        }
    }
}

fn prevent_sleep(pid: Option<u32>) -> SleepGuard {
    let Some(pid) = pid else {
        return SleepGuard { helper: None };
    };

    #[cfg(target_os = "macos")]
    let helper = std::process::Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();

    #[cfg(windows)]
    let helper = {
        let script = format!(
            r#"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SleepControl {{
    [DllImport("kernel32.dll")]
    public static extern uint SetThreadExecutionState(uint esFlags);
}}
'@
[SleepControl]::SetThreadExecutionState(0x80000001) | Out-Null
try {{ Wait-Process -Id {pid} -ErrorAction SilentlyContinue }}
finally {{ [SleepControl]::SetThreadExecutionState(0x80000000) | Out-Null }}
"#
        );
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
    };

    #[cfg(not(any(target_os = "macos", windows)))]
    let helper = None;

    SleepGuard { helper }
}

fn cookie_jar_has_entries(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|contents| {
            contents
                .lines()
                .any(|line| !line.is_empty() && !line.starts_with('#'))
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn secure_cookie_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).ok();
}

#[cfg(not(unix))]
fn secure_cookie_dir(_path: &std::path::Path) {}

#[cfg(unix)]
fn secure_cookie_file(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
}

#[cfg(not(unix))]
fn secure_cookie_file(_path: &std::path::Path) {}

async fn read_ytdlp_version(path: &std::path::Path) -> Result<String, String> {
    let mut command = tokio::process::Command::new(path);
    command.arg("--version").kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
        .await
        .map_err(|_| "yt-dlp version check timed out".to_string())?
        .map_err(|e| format!("Could not run yt-dlp: {e}"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if error.is_empty() {
            format!("yt-dlp exited with code {:?}", output.status.code())
        } else {
            error
        });
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        Err("yt-dlp returned an empty version".to_string())
    } else {
        Ok(version)
    }
}

async fn ffmpeg_is_working(path: &std::path::Path) -> bool {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(std::time::Duration::from_secs(10), command.status())
        .await
        .ok()
        .and_then(Result::ok)
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn fetch_latest_ytdlp_version() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Could not prepare update check: {e}"))?;
    let response = client
        .get("https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest")
        .header(
            reqwest::header::USER_AGENT,
            concat!("VideoDownloader/", env!("CARGO_PKG_VERSION")),
        )
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Update check failed: {e}"))?;
    let body = response
        .bytes()
        .await
        .map_err(|e| format!("Could not read update information: {e}"))?;
    let release: GithubRelease = serde_json::from_slice(&body)
        .map_err(|e| format!("Could not parse update information: {e}"))?;

    Ok(release.tag_name.trim_start_matches('v').to_string())
}

fn version_day(version: &str) -> Option<i64> {
    let date = version.trim_start_matches('v').get(..10)?;
    let mut parts = date.split('.');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era
}

async fn download_to_path(url: &str, dest: &std::path::Path) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let response = reqwest::get(url)
        .await
        .map_err(|e| format!("Could not download yt-dlp: {e}"))?
        .error_for_status()
        .map_err(|e| format!("yt-dlp download failed: {e}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("Cannot create update file: {e}"))?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("yt-dlp download failed: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Cannot write update file: {e}"))?;
    }

    file.flush()
        .await
        .map_err(|e| format!("Cannot finish update file: {e}"))
}

#[cfg(not(windows))]
async fn replace_ytdlp(current: &std::path::Path, update: &std::path::Path) -> Result<(), String> {
    tokio::fs::rename(update, current)
        .await
        .map_err(|e| format!("Could not install yt-dlp update: {e}"))
}

#[cfg(windows)]
async fn replace_ytdlp(current: &std::path::Path, update: &std::path::Path) -> Result<(), String> {
    let backup = current.with_extension("exe.backup");
    tokio::fs::remove_file(&backup).await.ok();
    tokio::fs::rename(current, &backup)
        .await
        .map_err(|e| format!("Could not prepare yt-dlp update: {e}"))?;

    if let Err(error) = tokio::fs::rename(update, current).await {
        tokio::fs::rename(&backup, current).await.ok();
        return Err(format!("Could not install yt-dlp update: {error}"));
    }

    tokio::fs::remove_file(backup).await.ok();
    Ok(())
}

async fn download_file(
    app: &AppHandle,
    url: &str,
    dest: &std::path::Path,
    start: f64,
    end: f64,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let resp = reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let total = resp.content_length().unwrap_or(0);
    let mut received: u64 = 0;

    let temp_dest = dest.with_extension("download");
    tokio::fs::remove_file(&temp_dest).await.ok();

    let mut file = tokio::fs::File::create(&temp_dest)
        .await
        .map_err(|e| e.to_string())?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                tokio::fs::remove_file(&temp_dest).await.ok();
                return Err(error.to_string());
            }
        };
        if let Err(error) = file.write_all(&chunk).await {
            tokio::fs::remove_file(&temp_dest).await.ok();
            return Err(error.to_string());
        }
        received += chunk.len() as u64;
        if total > 0 {
            let frac = received as f64 / total as f64;
            let p = start + frac * (end - start);
            app.emit("setup-progress", p).ok();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::remove_file(dest).await.ok();
    tokio::fs::rename(&temp_dest, dest)
        .await
        .map_err(|e| e.to_string())?;
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
            get_ytdlp_version,
            get_ytdlp_status,
            update_ytdlp,
            download_deps,
            get_default_output_path,
            start_download,
            cancel_download,
            open_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
