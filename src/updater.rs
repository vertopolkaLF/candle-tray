//! In-place updates from GitHub Releases portable zip assets, as in Codex
//! Minibar: check the latest release, download the zip for this
//! architecture, verify its SHA-256, and let a hidden PowerShell helper copy
//! it over the install once this process has exited, then relaunch.

use std::{
    fs,
    io::{Read, Write, copy},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, LazyLock, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use ureq::Agent;
use zip::ZipArchive;

pub const REPO_URL: &str = "https://github.com/vertopolkaLF/candle-tray";
pub const RELEASES_URL: &str = "https://github.com/vertopolkaLF/candle-tray/releases";
pub const ISSUES_URL: &str = "https://github.com/vertopolkaLF/candle-tray/issues";

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/vertopolkaLF/candle-tray/releases/latest";
const APP_EXE: &str = "candle-tray.exe";
const USER_AGENT: &str = "candle-tray-updater";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Candle Tray";
const UPDATE_SUCCESS_MARKER: &str = ".update-success-pending";
const UPDATE_STAGING_DIR: &str = ".update-staging";

/// Hard upper bound for a downloaded release package.
const MAX_UPDATE_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;
/// How often a running app looks for a new release.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Minimum spacing of progress repaints.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

fn http_agent() -> Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            // Use the OS TLS stack (schannel on Windows).
            let tls = ureq::native_tls::TlsConnector::new().expect("create native-tls connector");
            ureq::AgentBuilder::new()
                .tls_connector(Arc::new(tls))
                .timeout_connect(Duration::from_secs(15))
                .timeout_read(Duration::from_secs(30))
                .build()
        })
        .clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub asset_url: String,
    pub html_url: String,
    /// Asset size reported by the GitHub API, if any.
    pub size: Option<u64>,
    /// Expected lowercase hex SHA-256 reported by the GitHub API, if any.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available(AvailableUpdate),
    /// Downloading and unpacking; the fraction is known once the size is.
    Installing {
        version: String,
        progress: Option<f32>,
    },
    /// The helper is waiting for this process to exit.
    Restarting,
    /// The check failed; the message is for the log only.
    Failed,
}

struct State {
    phase: UpdatePhase,
    /// The last attempt to install failed; the update stays available.
    install_failed: bool,
    /// The popup offer was dismissed for this version.
    dismissed: Option<String>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        phase: UpdatePhase::Idle,
        install_failed: false,
        dismissed: None,
    })
});
static AUTO_CHECK: AtomicBool = AtomicBool::new(false);
static CHECKER_STARTED: AtomicBool = AtomicBool::new(false);
static CHECK_NOW: (Mutex<bool>, std::sync::Condvar) =
    (Mutex::new(false), std::sync::Condvar::new());

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_phase(phase: UpdatePhase) {
    state().phase = phase;
    repaint();
}

fn repaint() {
    crate::popup::send_command(crate::popup::PopupCommand::Run(Box::new(|cx| {
        cx.refresh_windows()
    })));
}

pub fn phase() -> UpdatePhase {
    state().phase.clone()
}

pub fn install_failed() -> bool {
    state().install_failed
}

/// The update to offer in the popup: available and not dismissed there.
pub fn popup_offer() -> Option<UpdatePhase> {
    let state = state();
    match &state.phase {
        UpdatePhase::Available(update)
            if state.dismissed.as_deref() != Some(update.version.as_str()) =>
        {
            Some(state.phase.clone())
        }
        UpdatePhase::Installing { .. } | UpdatePhase::Restarting => Some(state.phase.clone()),
        _ => None,
    }
}

/// Hide the popup offer for the available version until the next one.
pub fn dismiss_offer() {
    let mut state = state();
    if let UpdatePhase::Available(update) = &state.phase {
        state.dismissed = Some(update.version.clone());
    }
    drop(state);
    repaint();
}

/// Turn the automatic checks on or off. The first call starts the checker,
/// which looks right away and then once a day; turning it on checks now.
pub fn set_auto_check(enabled: bool) {
    let was = AUTO_CHECK.swap(enabled, Ordering::SeqCst);
    if !CHECKER_STARTED.swap(true, Ordering::SeqCst) {
        let spawned = thread::Builder::new()
            .name("update-checker".into())
            .spawn(checker_loop);
        if let Err(error) = spawned {
            crate::logger::error(format!("could not start the update checker: {error}"));
        }
    } else if enabled && !was {
        wake_checker();
    }
}

fn wake_checker() {
    let (lock, signal) = &CHECK_NOW;
    *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    signal.notify_one();
}

fn checker_loop() {
    // Startup already has enough to do; the release can wait a moment.
    thread::sleep(Duration::from_secs(5));
    loop {
        if AUTO_CHECK.load(Ordering::SeqCst) {
            check_once();
        }
        let (lock, signal) = &CHECK_NOW;
        let pending = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut pending, _) = signal
            .wait_timeout_while(pending, CHECK_INTERVAL, |pending| !*pending)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = false;
    }
}

/// Check now, from the About page. Never blocks the caller.
pub fn check_now() {
    if matches!(
        phase(),
        UpdatePhase::Checking | UpdatePhase::Installing { .. } | UpdatePhase::Restarting
    ) {
        return;
    }
    let spawned = thread::Builder::new()
        .name("update-check".into())
        .spawn(check_once);
    if let Err(error) = spawned {
        crate::logger::error(format!("could not check for updates: {error}"));
    }
}

fn check_once() {
    if matches!(
        phase(),
        UpdatePhase::Installing { .. } | UpdatePhase::Restarting
    ) {
        return;
    }
    set_phase(UpdatePhase::Checking);
    match check_for_update() {
        Ok(Some(update)) => {
            crate::logger::info(format!("Update available: {}", update.version));
            set_phase(UpdatePhase::Available(update));
        }
        Ok(None) => set_phase(UpdatePhase::UpToDate),
        Err(error) => {
            crate::logger::error(format!("update check failed: {error:#}"));
            set_phase(UpdatePhase::Failed);
        }
    }
}

/// Download and install the available update, then exit so the helper can
/// replace the files. Runs in the background; progress shows in the UI.
pub fn install() {
    let update = {
        let mut state = state();
        let UpdatePhase::Available(update) = state.phase.clone() else {
            return;
        };
        state.install_failed = false;
        state.phase = UpdatePhase::Installing {
            version: update.version.clone(),
            progress: None,
        };
        update
    };
    repaint();
    let spawned = thread::Builder::new()
        .name("update-install".into())
        .spawn(move || match apply_update(&update) {
            Ok(()) => {
                set_phase(UpdatePhase::Restarting);
                crate::logger::info(format!("Restarting to install {}", update.version));
                // Let the last step show before the process goes away; the
                // helper waits for this process to exit anyway.
                thread::sleep(Duration::from_millis(600));
                crate::tray::request_exit();
            }
            Err(error) => {
                crate::logger::error(format!("update failed: {error:#}"));
                let mut state = state();
                state.install_failed = true;
                state.phase = UpdatePhase::Available(update);
                drop(state);
                repaint();
            }
        });
    if let Err(error) = spawned {
        crate::logger::error(format!("could not start the update: {error}"));
        let mut state = state();
        state.install_failed = true;
        if let UpdatePhase::Installing { .. } = state.phase {
            state.phase = UpdatePhase::Idle;
        }
    }
}

fn report_progress(version: &str, progress: Option<f32>) {
    set_phase(UpdatePhase::Installing {
        version: version.to_owned(),
        progress,
    });
}

/// Rate-limits progress repaints; the final one always goes through.
struct ProgressThrottle(Option<Instant>);

impl ProgressThrottle {
    fn ready(&mut self, done: bool) -> bool {
        let now = Instant::now();
        if done
            || self
                .0
                .is_none_or(|last| now.duration_since(last) >= PROGRESS_INTERVAL)
        {
            self.0 = Some(now);
            return true;
        }
        false
    }
}

/// Startup housekeeping: logs a finished update, keeps the "Installed apps"
/// version current and removes leftover staging.
pub fn startup() {
    match take_post_update_marker() {
        Ok(Some(version)) => crate::logger::info(format!("Updated to {version}")),
        Ok(None) => {}
        Err(error) => crate::logger::error(format!("could not read the update marker: {error:#}")),
    }
    #[cfg(windows)]
    if let Err(error) = sync_uninstall_display_version(env!("CARGO_PKG_VERSION")) {
        crate::logger::error(format!("could not update the installed version: {error:#}"));
    }
    if let Ok(dir) = install_dir() {
        remove_update_staging(&dir.join(UPDATE_STAGING_DIR));
    }
}

/// Zip updates replace files but not the installer's uninstall entry.
#[cfg(windows)]
fn sync_uninstall_display_version(version: &str) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegOpenKeyExW,
            RegSetValueExW,
        },
    };
    let wide = |value: &str| -> Vec<u16> { value.encode_utf16().chain([0]).collect() };
    let subkey = wide(UNINSTALL_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated strings and an out pointer to a local.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        // A portable copy has no uninstall entry.
        return Ok(());
    }
    let name = wide("DisplayVersion");
    let data = wide(version);
    // SAFETY: `key` is open; the data is a NUL-terminated UTF-16 string.
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * size_of::<u16>()) as u32,
        )
    };
    // SAFETY: closes the key opened above.
    unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        bail!("RegSetValueExW(DisplayVersion): {status}");
    }
    Ok(())
}

/// Drops staging left by finished or failed updates, keeping this process's
/// own run directory in case an update is being applied right now.
fn remove_update_staging(staging: &Path) -> usize {
    let Ok(entries) = fs::read_dir(staging) else {
        return 0;
    };
    let own = format!("run-{}", std::process::id());
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name() != own.as_str() {
            removed += usize::from(fs::remove_dir_all(entry.path()).is_ok());
        }
    }
    removed + usize::from(fs::remove_dir(staging).is_ok())
}

fn take_post_update_marker() -> Result<Option<String>> {
    let path = install_dir()?.join(UPDATE_SUCCESS_MARKER);
    if !path.exists() {
        return Ok(None);
    }
    let version = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .to_string();
    let _ = fs::remove_file(&path);
    Ok((!version.is_empty()).then_some(version))
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    digest: Option<String>,
}

/// Extracts the lowercase hex SHA-256 from a GitHub `digest` field.
fn parse_sha256_digest(digest: &str) -> Option<String> {
    let hex = digest.trim().strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    }
}

fn check_for_update() -> Result<Option<AvailableUpdate>> {
    let release: GhRelease = github_get(LATEST_RELEASE_API)?;
    let remote = parse_release_version(&release.tag_name)?;
    if remote <= current_version() {
        return Ok(None);
    }
    let suffix = format!("-{}-portable.zip", host_arch());
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(&suffix))
        .with_context(|| {
            format!(
                "release {} has no portable zip (*{suffix})",
                release.tag_name
            )
        })?;
    Ok(Some(AvailableUpdate {
        version: remote.to_string(),
        asset_url: asset.browser_download_url.clone(),
        html_url: release.html_url,
        size: asset.size,
        sha256: asset.digest.as_deref().and_then(parse_sha256_digest),
    }))
}

fn parse_release_version(tag: &str) -> Result<Version> {
    let trimmed = tag.trim();
    // Accept "v1.2.3", "V1.2.3", "v.1.2.3" and bare "1.2.3".
    let bare = trimmed
        .strip_prefix("v.")
        .or_else(|| trimmed.strip_prefix("V."))
        .or_else(|| trimmed.strip_prefix('v'))
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
        .trim_start_matches('.');
    Version::parse(bare).with_context(|| format!("parse release tag {tag:?}"))
}

fn github_get(url: &str) -> Result<GhRelease> {
    let response = http_agent()
        .get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    let body = response
        .into_string()
        .with_context(|| format!("read GitHub response body ({status})"))?;
    if status / 100 != 2 {
        bail!("GitHub API returned {status}: {body}");
    }
    serde_json::from_str(&body).context("parse GitHub release JSON")
}

fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    exe.parent()
        .map(Path::to_path_buf)
        .context("executable has no parent directory")
}

fn install_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".candle-tray-update-write-test");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn apply_update(update: &AvailableUpdate) -> Result<()> {
    let install_dir = install_dir()?;
    if !install_dir_writable(&install_dir) {
        open_url(&update.html_url);
        bail!(
            "cannot update in place because {} is not writable; opened the release page",
            install_dir.display()
        );
    }

    let run_dir = install_dir
        .join(UPDATE_STAGING_DIR)
        .join(format!("run-{}", std::process::id()));
    if run_dir.exists() {
        fs::remove_dir_all(&run_dir)
            .with_context(|| format!("clear previous update staging at {}", run_dir.display()))?;
    }
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("create update staging at {}", run_dir.display()))?;

    let zip_path = run_dir.join("package.zip");
    let limit = update.size.map_or(MAX_UPDATE_DOWNLOAD_BYTES, |size| {
        size.min(MAX_UPDATE_DOWNLOAD_BYTES)
    });
    let mut throttle = ProgressThrottle(None);
    let digest = download_file_limited(
        &update.asset_url,
        &zip_path,
        limit,
        &mut |downloaded, length| {
            let total = update.size.or(length).filter(|total| *total > 0);
            if throttle.ready(total == Some(downloaded)) {
                report_progress(
                    &update.version,
                    total.map(|total| downloaded.min(total) as f32 / total as f32),
                );
            }
        },
    )?;
    if let Some(expected) = &update.sha256
        && !digest.eq_ignore_ascii_case(expected)
    {
        let _ = fs::remove_file(&zip_path);
        bail!("update package checksum mismatch (expected {expected}, got {digest})");
    }
    let extracted = run_dir.join("extracted");
    fs::create_dir_all(&extracted).context("create extraction directory")?;
    let payload_root = extract_portable_zip(&zip_path, &extracted)?;
    if !payload_root.join(APP_EXE).is_file() {
        bail!("update package has no {APP_EXE}");
    }

    let pid = std::process::id();
    let script_path = run_dir.join("apply-update.ps1");
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$pidToWait = {pid}
$install = '{install}'
$source = '{source}'
$staging = '{staging}'
$exe = Join-Path $install '{APP_EXE}'

while (Get-Process -Id $pidToWait -ErrorAction SilentlyContinue) {{
    Start-Sleep -Milliseconds 250
}}

try {{
    Get-ChildItem -LiteralPath $source -Force | ForEach-Object {{
        Copy-Item -LiteralPath $_.FullName -Destination $install -Recurse -Force
    }}
    Set-Content -LiteralPath (Join-Path $install '{marker}') -Value '{version}' -Encoding utf8 -NoNewline
    $key = 'HKCU:\{uninstall}'
    if (Test-Path -LiteralPath $key) {{
        Set-ItemProperty -LiteralPath $key -Name DisplayVersion -Value '{version}'
    }}
}} finally {{
    Start-Process -FilePath $exe -WorkingDirectory $install
    Start-Sleep -Seconds 2
    if (Test-Path -LiteralPath $staging) {{
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }}
}}
"#,
        install = escape_ps(&install_dir.display().to_string()),
        source = escape_ps(&payload_root.display().to_string()),
        staging = escape_ps(&run_dir.display().to_string()),
        marker = UPDATE_SUCCESS_MARKER,
        uninstall = UNINSTALL_KEY,
        version = escape_ps(&update.version),
    );
    fs::write(&script_path, script).context("write update helper script")?;
    spawn_hidden_helper(&script_path).context("spawn update helper")?;
    Ok(())
}

#[cfg(windows)]
fn spawn_hidden_helper(script_path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .context("spawn hidden PowerShell update helper")
}

#[cfg(not(windows))]
fn spawn_hidden_helper(_script_path: &Path) -> Result<()> {
    bail!("in-place updates are only supported on Windows")
}

fn escape_ps(value: &str) -> String {
    value.replace('\'', "''")
}

/// Downloads `url` to `destination`, failing if more than `max_bytes` arrive.
/// Reports `(downloaded, Content-Length)` as bytes arrive and returns the
/// lowercase hex SHA-256 of the written bytes.
fn download_file_limited(
    url: &str,
    destination: &Path,
    max_bytes: u64,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String> {
    let response = http_agent()
        .get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("download {url}"))?;
    let status = response.status();
    if status / 100 != 2 {
        bail!("download failed with status {status}");
    }
    let length = response
        .header("Content-Length")
        .and_then(|value| value.trim().parse::<u64>().ok());
    let reader = response.into_reader();
    let mut file = fs::File::create(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    let result = copy_limited(reader, &mut file, max_bytes, &mut |downloaded| {
        on_progress(downloaded, length)
    });
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

/// Copies at most `max_bytes` from `reader`, erroring if more are available.
/// Reports the running total after every chunk and returns the hex SHA-256
/// of the copied bytes.
fn copy_limited(
    reader: impl Read,
    writer: &mut impl Write,
    max_bytes: u64,
    on_progress: &mut dyn FnMut(u64),
) -> Result<String> {
    let mut limited = reader.take(max_bytes.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = limited
            .read(&mut buf)
            .context("read downloaded update package")?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            bail!("download exceeds the {max_bytes} byte limit");
        }
        hasher.update(&buf[..read]);
        writer
            .write_all(&buf[..read])
            .context("write downloaded update package")?;
        on_progress(total);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn extract_portable_zip(archive_path: &Path, destination: &Path) -> Result<PathBuf> {
    let file =
        fs::File::open(archive_path).with_context(|| format!("open {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file).context("open update zip archive")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("read zip entry")?;
        // Entries escaping the destination are skipped.
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let target = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let mut output =
            fs::File::create(&target).with_context(|| format!("create {}", target.display()))?;
        copy(&mut entry, &mut output).with_context(|| format!("extract {}", target.display()))?;
    }
    payload_root_from_extracted(destination)
}

/// A zip holding a single folder ships its files inside it.
fn payload_root_from_extracted(destination: &Path) -> Result<PathBuf> {
    let mut entries = fs::read_dir(destination)
        .with_context(|| format!("read {}", destination.display()))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    if entries.len() == 1 {
        let only = entries.remove(0);
        if only
            .file_type()
            .context("inspect extracted entry")?
            .is_dir()
        {
            return Ok(only.path());
        }
    }
    Ok(destination.to_path_buf())
}

/// Opens a URL in the browser on a worker thread: ShellExecuteW pumps
/// messages, which must not happen inside a GPUI click handler.
pub fn open_url(url: &str) {
    let url = url.to_owned();
    let spawned = thread::Builder::new()
        .name("shell-open".into())
        .spawn(move || {
            if let Err(error) = shell_open(&url) {
                crate::logger::error(format!("{error:#}"));
            }
        });
    if let Err(error) = spawned {
        crate::logger::error(format!("could not open {error}"));
    }
}

#[cfg(windows)]
fn shell_open(url: &str) -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let wide = |value: &str| -> Vec<u16> { value.encode_utf16().chain([0]).collect() };
    let operation = wide("open");
    let target = wide(url);
    // SAFETY: valid NUL-terminated strings; SW_SHOWNORMAL.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        bail!("ShellExecuteW failed for {url}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn shell_open(_url: &str) -> Result<()> {
    bail!("opening URLs is only supported on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_limited_enforces_cap_and_hashes() {
        let mut out = Vec::new();
        let digest = copy_limited(&b"abc"[..], &mut out, 3, &mut |_| {}).unwrap();
        assert_eq!(out, b"abc");
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut out = Vec::new();
        assert!(copy_limited(&b"abcd"[..], &mut out, 3, &mut |_| {}).is_err());
    }

    #[test]
    fn parses_sha256_digest() {
        let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            parse_sha256_digest(&format!("sha256:{hex}")).as_deref(),
            Some(hex)
        );
        assert_eq!(parse_sha256_digest("sha1:abc"), None);
        assert_eq!(parse_sha256_digest("sha256:zz"), None);
    }

    #[test]
    fn parses_release_tags() {
        for tag in ["v1.2.3", "V1.2.3", "v.1.2.3", "1.2.3", "  v1.2.3  "] {
            assert_eq!(parse_release_version(tag).unwrap(), Version::new(1, 2, 3));
        }
        assert!(parse_release_version("latest").is_err());
    }

    #[test]
    fn staging_cleanup_keeps_the_running_update() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join(UPDATE_STAGING_DIR);
        let own = staging.join(format!("run-{}", std::process::id()));
        fs::create_dir_all(staging.join("run-1")).unwrap();
        fs::create_dir_all(&own).unwrap();
        remove_update_staging(&staging);
        assert!(own.exists());
        assert!(!staging.join("run-1").exists());
        fs::remove_dir_all(&own).unwrap();
        remove_update_staging(&staging);
        assert!(!staging.exists());
    }
}
