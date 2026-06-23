// Tauri v2 globals (withGlobalTauri: true in tauri.conf.json)
const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const listen = tauri?.event?.listen;
const openDialog = tauri?.dialog?.open;

// ── DOM refs ─────────────────────────────────────────────────
const setupScreen      = document.getElementById("setup-screen");
const mainScreen       = document.getElementById("main-screen");
const setupTaskLabel   = document.getElementById("setup-task-label");
const setupProgressFill= document.getElementById("setup-progress-fill");
const setupProgressPct = document.getElementById("setup-progress-pct");
const setupError       = document.getElementById("setup-error");
const setupRetryBtn    = document.getElementById("setup-retry-btn");

const urlInput         = document.getElementById("url-input");
const pasteBtn         = document.getElementById("paste-btn");
const qualitySelect    = document.getElementById("quality-select");
const cookieSelect     = document.getElementById("cookie-select");
const outputDisplay    = document.getElementById("output-display");
const folderBtn        = document.getElementById("folder-btn");
const downloadBtn      = document.getElementById("download-btn");
const cancelBtn        = document.getElementById("cancel-btn");
const ytdlpVersion     = document.getElementById("ytdlp-version");
const ytdlpUpdateStatus= document.getElementById("ytdlp-update-status");
const updateYtdlpBtn   = document.getElementById("update-ytdlp-btn");

const dlProgressFill   = document.getElementById("dl-progress-fill");
const dlStatus         = document.getElementById("dl-status");
const dlPct            = document.getElementById("dl-pct");
const logOutput        = document.getElementById("log-output");
const openFolderBtn    = document.getElementById("open-folder-btn");

// ── State ─────────────────────────────────────────────────────
let outputPath = "";
let isDownloading = false;
let isUpdatingYtdlp = false;
let ytdlpManagedByApp = true;
let hadAnyDownload = false;

// ── Init ──────────────────────────────────────────────────────
async function init() {
  showSetup();
  setupTaskLabel.textContent = "Checking required tools…";

  if (!invoke || !listen) {
    throw new Error("The application bridge did not load. Please reinstall the app.");
  }

  await registerListeners();

  const hasDeps = await invoke("check_deps");
  if (hasDeps) {
    await showMain();
  } else {
    showSetup();
    await runSetup();
  }
}

// ── Setup screen ──────────────────────────────────────────────
function showSetup() {
  setupScreen.classList.remove("hidden");
  mainScreen.classList.add("hidden");
}

function showSetupError(msg) {
  setupError.textContent = "Error: " + msg;
  setupError.classList.remove("hidden");
  setupRetryBtn.classList.remove("hidden");
}

function showStartupError(error) {
  showSetup();
  setupTaskLabel.textContent = "Video Downloader could not start";
  setupProgressFill.style.width = "0%";
  setupProgressPct.textContent = "";
  setupRetryBtn.classList.add("hidden");
  setupError.textContent = "Error: " + String(error);
  setupError.classList.remove("hidden");
}

async function runSetup() {
  setupRetryBtn.classList.add("hidden");
  setupRetryBtn.disabled = true;
  setupError.classList.add("hidden");
  setupError.textContent = "";
  setupTaskLabel.textContent = "Preparing…";
  setupProgressFill.style.width = "0%";
  setupProgressPct.textContent = "0%";

  try {
    await invoke("download_deps");
  } catch (err) {
    showSetupError(String(err));
  } finally {
    setupRetryBtn.disabled = false;
  }
}

async function showMain() {
  setupScreen.classList.add("hidden");
  mainScreen.classList.remove("hidden");

  // Fill default output path
  outputPath = await invoke("get_default_output_path");
  outputDisplay.value = outputPath;
  await refreshYtdlpStatus();
}

// ── Event listeners (Tauri backend → frontend) ────────────────
async function registerListeners() {
  await listen("setup-task", (event) => {
    setupTaskLabel.textContent = event.payload;
  });

  await listen("setup-progress", (event) => {
    const pct = Math.round(event.payload * 100);
    setupProgressFill.style.width = pct + "%";
    setupProgressPct.textContent  = pct + "%";
  });

  await listen("setup-done", async () => {
    setupProgressFill.style.width = "100%";
    setupProgressPct.textContent  = "100%";
    // Small delay so the user sees 100%
    await delay(400);
    await showMain();
  });

  await listen("download-log", (event) => {
    appendLog(event.payload);
    if (event.payload.includes("[download] 100%")) {
      hadAnyDownload = true;
    }
    if (event.payload.includes("Could not copy Chrome cookie database")) {
      dlStatus.textContent = "Close Chrome completely and try again, or switch to Firefox cookies.";
      dlStatus.style.color = "var(--error)";
    }
  });

  await listen("download-progress", (event) => {
    const pct = Math.round(event.payload * 100);
    dlProgressFill.style.width = pct + "%";
    dlPct.textContent = pct + "%";
  });

  await listen("download-complete", () => {
    setDownloading(false);
    dlProgressFill.style.width = "100%";
    dlPct.textContent = "100%";
    dlStatus.textContent = "Complete ✓";
    dlStatus.style.color = "var(--success)";
    openFolderBtn.classList.remove("hidden");
  });

  await listen("download-error", (event) => {
    setDownloading(false);
    dlStatus.textContent = "Error: " + event.payload;
    dlStatus.style.color = "var(--error)";
    if (hadAnyDownload) {
      openFolderBtn.classList.remove("hidden");
    }
  });
}

// ── UI interactions ───────────────────────────────────────────
pasteBtn.addEventListener("click", async () => {
  try {
    const text = await navigator.clipboard.readText();
    urlInput.value = text.trim();
  } catch {
    // clipboard read may be denied; ignore
  }
});

folderBtn.addEventListener("click", async () => {
  if (!openDialog) {
    dlStatus.textContent = "Folder picker is unavailable. Restart or reinstall the app.";
    dlStatus.style.color = "var(--error)";
    return;
  }
  const selected = await openDialog({ directory: true, multiple: false, title: "Choose save folder" });
  if (selected) {
    outputPath = selected;
    outputDisplay.value = outputPath;
  }
});

setupRetryBtn.addEventListener("click", runSetup);

downloadBtn.addEventListener("click", async () => {
  const url = urlInput.value.trim();
  if (!url) { urlInput.focus(); return; }
  if (!outputPath) { return; }

  // Reset UI
  resetDownloadUI();
  setDownloading(true);

  const formatArgs = buildFormatArgs(qualitySelect.value);
  const cookieBrowser = cookieSelect.value;

  try {
    await invoke("start_download", {
      url,
      formatArgs,
      outputPath,
      cookieBrowser,
    });
    dlStatus.textContent = "Downloading…";
  } catch (err) {
    setDownloading(false);
    dlStatus.textContent = "Error: " + err;
    dlStatus.style.color = "var(--error)";
  }
});

cancelBtn.addEventListener("click", async () => {
  await invoke("cancel_download");
  setDownloading(false);
  dlStatus.textContent = "Cancelled";
  dlStatus.style.color = "var(--muted)";
});

openFolderBtn.addEventListener("click", async () => {
  await invoke("open_folder", { path: outputPath });
});

updateYtdlpBtn.addEventListener("click", async () => {
  if (!ytdlpManagedByApp) {
    return;
  }
  isUpdatingYtdlp = true;
  updateYtdlpBtn.disabled = true;
  downloadBtn.disabled = true;
  updateYtdlpBtn.textContent = "Checking…";
  ytdlpUpdateStatus.textContent = "Downloading and validating the latest release…";
  ytdlpUpdateStatus.style.color = "var(--muted)";

  try {
    const result = await invoke("update_ytdlp");
    ytdlpVersion.textContent = "Version: " + result.current_version;
    if (result.updated) {
      ytdlpUpdateStatus.textContent =
        `Updated from ${result.previous_version} to ${result.current_version}.`;
    } else {
      ytdlpUpdateStatus.textContent = "Already up to date.";
    }
    ytdlpUpdateStatus.style.color = "var(--success)";
  } catch (err) {
    ytdlpUpdateStatus.textContent = "Update failed: " + err;
    ytdlpUpdateStatus.style.color = "var(--error)";
  } finally {
    isUpdatingYtdlp = false;
    updateYtdlpBtn.disabled = isDownloading || !ytdlpManagedByApp;
    downloadBtn.disabled = isDownloading;
    updateYtdlpBtn.textContent = "Update yt-dlp";
  }
});

// ── Helpers ───────────────────────────────────────────────────
async function refreshYtdlpStatus() {
  try {
    const status = await invoke("get_ytdlp_status");
    ytdlpManagedByApp = status.managed_by_app;
    ytdlpVersion.textContent =
      `Version: ${status.current_version} · ${status.managed_by_app ? "App managed" : "System installation"}`;
    ytdlpVersion.title = status.install_path;
    updateYtdlpBtn.disabled = !status.managed_by_app;
    updateYtdlpBtn.textContent = status.managed_by_app ? "Update yt-dlp" : "Managed externally";

    if (status.update_available) {
      const age = status.days_outdated == null
        ? ""
        : ` · ${status.days_outdated} ${status.days_outdated === 1 ? "day" : "days"} outdated`;
      ytdlpUpdateStatus.textContent =
        `Update available: ${status.latest_version}${age}` +
        (status.managed_by_app ? "" : " · Update it with your package manager.");
      ytdlpUpdateStatus.style.color = "var(--warning)";
    } else if (status.latest_version) {
      ytdlpUpdateStatus.textContent = "Up to date.";
      ytdlpUpdateStatus.style.color = "var(--success)";
    } else {
      ytdlpUpdateStatus.textContent = "Could not check for updates.";
      ytdlpUpdateStatus.style.color = "var(--muted)";
    }
  } catch (err) {
    ytdlpManagedByApp = true;
    ytdlpVersion.textContent = "Version unavailable";
    ytdlpUpdateStatus.textContent = String(err);
    ytdlpUpdateStatus.style.color = "var(--error)";
  }
}

function buildFormatArgs(value) {
  if (value === "mp3") {
    return ["-x", "--audio-format", "mp3"];
  }
  if (value === "audio") {
    return ["-f", "bestaudio/best"];
  }

  const heightFilter = value === "best" ? "" : `[height<=${value}]`;
  const compatibleFormat = [
    `bestvideo${heightFilter}[vcodec^=avc1]+bestaudio[ext=m4a]`,
    `bestvideo${heightFilter}[vcodec^=avc1]+bestaudio`,
    `best${heightFilter}[vcodec^=avc1][ext=mp4]`,
    `best${heightFilter}[vcodec^=avc1]`,
  ].join("/");

  return ["-f", compatibleFormat];
}

function resetDownloadUI() {
  dlProgressFill.style.width = "0%";
  dlPct.textContent = "0%";
  dlStatus.textContent = "Starting…";
  dlStatus.style.color = "";
  logOutput.textContent = "";
  openFolderBtn.classList.add("hidden");
  hadAnyDownload = false;
}

function setDownloading(active) {
  isDownloading = active;
  downloadBtn.classList.toggle("hidden", active);
  cancelBtn.classList.toggle("hidden", !active);
  downloadBtn.disabled = active || isUpdatingYtdlp;
  updateYtdlpBtn.disabled = active || !ytdlpManagedByApp;
}

function appendLog(line) {
  logOutput.textContent += line + "\n";
  logOutput.scrollTop = logOutput.scrollHeight;
}

function delay(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

// ── Start ─────────────────────────────────────────────────────
init().catch(showStartupError);
