//! Self-update: daily GitHub release check plus `px0 --update`.
//!
//! Ports `update.go`: the same state file, exact-match asset names in Go
//! `GOOS/GOARCH` vocabulary, `checksums.txt` verification, and the
//! atomic binary swap (rename, copy fallback, Windows `.old` dance).

use std::path::PathBuf;

pub const DEFAULT_REPO: &str = "eas4ai/px0-rust";
/// Minimum age of the state file before we check again. Ports Go
/// `updateCheckPeriod` (24 h).
pub const UPDATE_CHECK_PERIOD_SECS: i64 = 24 * 3600;

/// Blocking HTTP client with a global deadline. ureq v3 carries
/// timeouts on the agent config rather than per request.
pub(crate) fn http_agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
        .build()
        .new_agent()
}

/// `PX0_REPO` override, else the default. Ports Go `getRepoName`.
pub fn repo_name() -> String {
    let r = std::env::var("PX0_REPO").unwrap_or_default();
    if r.trim().is_empty() {
        DEFAULT_REPO.to_string()
    } else {
        r.trim().to_string()
    }
}

/// `XDG_STATE_HOME/px0/update_check.json`, else
/// `~/.px0/update_check.json`, else the temp dir. Ports Go
/// `stateFilePath`.
pub fn state_file_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("px0").join("update_check.json");
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home).join(".px0").join("update_check.json"),
        _ => std::env::temp_dir().join("px0_update_check.json"),
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UpdateState {
    pub last_checked: String,
    pub latest_ver: String,
}

pub fn read_update_state() -> Result<UpdateState, String> {
    let data = std::fs::read(state_file_path()).map_err(|e| e.to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

pub fn write_update_state(state: &UpdateState) {
    let path = state_file_path();
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(data) = serde_json::to_vec(state) {
        let _ = std::fs::write(path, data);
    }
}

/// Numeric dotted-version compare, ignoring a leading `v` and any
/// `-suffix` per component. Ports Go `compareSemver` exactly, including
/// its `-1/0/1` return contract.
pub fn compare_semver(v1: &str, v2: &str) -> i32 {
    let v1 = v1.trim().strip_prefix('v').unwrap_or(v1.trim());
    let v2 = v2.trim().strip_prefix('v').unwrap_or(v2.trim());
    let p1: Vec<&str> = v1.split('.').collect();
    let p2: Vec<&str> = v2.split('.').collect();
    let n = p1.len().max(p2.len());
    for i in 0..n {
        let num = |p: &[&str]| {
            p.get(i)
                .map(|s| {
                    s.split('-')
                        .next()
                        .unwrap_or("")
                        .parse::<i64>()
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        };
        let (a, b) = (num(&p1), num(&p2));
        if a > b {
            return 1;
        }
        if a < b {
            return -1;
        }
    }
    0
}

/// `std` OS/arch names in Go's `GOOS/GOARCH` vocabulary, which is what
/// release assets are named with.
pub fn go_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    }
}

pub fn go_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    }
}

/// Exact asset file name for a version without its `v` prefix. Ports Go
/// `expectedAsset` in `runSelfUpdate`.
pub fn expected_asset_name(version_no_v: &str) -> String {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("px0-{version_no_v}-{}-{}{ext}", go_os(), go_arch())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub assets: Vec<GithubAsset>,
}

/// Query the GitHub API (or `PX0_UPDATE_URL`) for the latest release.
/// Ports Go `fetchLatestRelease`: 5 s client, `px0-updater` agent, the
/// 404 "no published releases" message, and `HTTP %d from %s` otherwise.
pub fn fetch_latest_release(repo: &str) -> Result<GithubRelease, String> {
    let api_url = std::env::var("PX0_UPDATE_URL").unwrap_or_default();
    let api_url = if api_url.is_empty() {
        format!("https://api.github.com/repos/{repo}/releases/latest")
    } else {
        api_url
    };
    let resp = http_agent(5)
        .get(&api_url)
        .header("User-Agent", "px0-updater")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| format_github_error(&e, &api_url, repo))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Err(format!("no published releases found for {repo} yet"));
    }
    if status != 200 {
        return Err(format!("HTTP {status} from {api_url}"));
    }
    resp.into_body()
        .read_json::<GithubRelease>()
        .map_err(|e| e.to_string())
}

fn format_github_error(e: &ureq::Error, api_url: &str, repo: &str) -> String {
    match e {
        ureq::Error::StatusCode(code) => {
            if *code == 404 {
                format!("no published releases found for {repo} yet")
            } else {
                format!("HTTP {code} from {api_url}")
            }
        }
        other => other.to_string(),
    }
}

/// GET a URL, demanding 200. Ports Go `downloadAsset`.
pub fn download_asset(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = http_agent(60)
        .get(url)
        .header("User-Agent", "px0-updater")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(code) => format!("HTTP {code} from {url}"),
            other => other.to_string(),
        })?;
    if resp.status().as_u16() != 200 {
        return Err(format!("HTTP {} from {url}", resp.status().as_u16()));
    }
    resp.body_mut().read_to_vec().map_err(|e| e.to_string())
}

/// Find `assetName`'s SHA-256 in a `checksums.txt` body. Ports Go
/// `checksumFor`: `*`/​`./` prefixes stripped, duplicates and malformed
/// hex rejected.
pub fn checksum_for(data: &[u8], asset_name: &str) -> Result<String, String> {
    let text = String::from_utf8_lossy(data);
    let mut checksum = String::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        // One `*` then one `./`, exactly like Go's nested TrimPrefix.
        let mut name = fields[1];
        if let Some(s) = name.strip_prefix('*') {
            name = s;
        }
        if let Some(s) = name.strip_prefix("./") {
            name = s;
        }
        if name != asset_name {
            continue;
        }
        if !checksum.is_empty() {
            return Err(format!("multiple checksums found for {asset_name}"));
        }
        let hex_ok = fields[0].len() == 64 && fields[0].bytes().all(|b| b.is_ascii_hexdigit());
        if !hex_ok {
            return Err(format!("invalid checksum for {asset_name}"));
        }
        checksum = fields[0].to_lowercase();
    }
    if checksum.is_empty() {
        return Err(format!("checksum not found for {asset_name}"));
    }
    Ok(checksum)
}

/// SHA-256 hex digest of a buffer.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Download an asset after verifying it against `checksums.txt`.
/// Ports Go `downloadVerifiedAsset`.
pub fn download_verified_asset(
    asset_url: &str,
    checksum_url: &str,
    asset_name: &str,
) -> Result<Vec<u8>, String> {
    let checksums = download_asset(checksum_url).map_err(|e| format!("download checksums: {e}"))?;
    let expected = checksum_for(&checksums, asset_name)?;
    let data = download_asset(asset_url).map_err(|e| format!("download binary: {e}"))?;
    if sha256_hex(&data) != expected {
        return Err(format!("checksum mismatch for {asset_name}"));
    }
    Ok(data)
}

/// Parse our own RFC3339 timestamps back to epoch seconds. Accepts the
/// `YYYY-MM-DDTHH:MM:SS[.frac][Z|±hh:mm]` shapes we write; anything
/// else is an error, which the caller treats as "check now".
fn parse_rfc3339_epoch(s: &str) -> Result<i64, ()> {
    let days_from_civil = |y: i64, m: i64, d: i64| {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    let t = s.trim();
    let (date, rest) = t.split_once(['T', 't']).ok_or(())?;
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return Err(());
    }
    let (y, mo, d): (i64, i64, i64) = (
        dp[0].parse().map_err(|_| ())?,
        dp[1].parse().map_err(|_| ())?,
        dp[2].parse().map_err(|_| ())?,
    );
    // Split the zone suffix off the clock.
    let (clock, zone) = if let Some(c) = rest.strip_suffix(['Z', 'z']) {
        (c, 0i64)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, z) = rest.split_at(i);
        let neg = z.starts_with('-');
        let zp: Vec<&str> = z[1..].split(':').collect();
        if zp.len() != 2 {
            return Err(());
        }
        let zh: i64 = zp[0].parse().map_err(|_| ())?;
        let zm: i64 = zp[1].parse().map_err(|_| ())?;
        (
            c,
            if neg {
                -(zh * 3600 + zm * 60)
            } else {
                zh * 3600 + zm * 60
            },
        )
    } else {
        return Err(());
    };
    let clock = clock.split('.').next().unwrap_or(clock);
    let cp: Vec<&str> = clock.split(':').collect();
    if cp.len() != 3 {
        return Err(());
    }
    let (h, mi, sec): (i64, i64, i64) = (
        cp[0].parse().map_err(|_| ())?,
        cp[1].parse().map_err(|_| ())?,
        cp[2].parse().map_err(|_| ())?,
    );
    Ok(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - zone)
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One background update check. Ports Go `checkDailyUpdate`: silent in
/// quiet mode, at most once a day, re-printing a known-newer version.
/// Returns the notification line when the user should see one.
pub fn check_daily_update(current_version: &str, quiet: bool) -> Option<String> {
    if quiet {
        return None;
    }
    let now = epoch_now();
    if let Ok(state) = read_update_state() {
        let fresh = parse_rfc3339_epoch(&state.last_checked)
            .map(|t| now - t < UPDATE_CHECK_PERIOD_SECS)
            .unwrap_or(false);
        if fresh {
            if !state.latest_ver.is_empty()
                && compare_semver(&state.latest_ver, current_version) > 0
            {
                return Some(update_notification(&state.latest_ver, current_version));
            }
            return None;
        }
    }
    let rel = fetch_latest_release(&repo_name()).ok()?;
    let latest_ver = rel
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&rel.tag_name)
        .to_string();
    write_update_state(&UpdateState {
        last_checked: crate::telemetry::rfc3339_now(),
        latest_ver: latest_ver.clone(),
    });
    if compare_semver(&latest_ver, current_version) > 0 {
        Some(update_notification(&latest_ver, current_version))
    } else {
        None
    }
}

fn update_notification(latest_ver: &str, current_version: &str) -> String {
    format!(
        "a new version of px0 (v{latest_ver}) is available (current: v{current_version}): run 'px0 --update' to upgrade"
    )
}

/// Locate the named asset in a release, else fall back to the standard
/// GitHub download URL pattern. Ports the `runSelfUpdate` lookup.
pub fn asset_download_url(
    rel: &GithubRelease,
    repo: &str,
    asset_name: &str,
) -> (String, Option<String>) {
    let mut download = String::new();
    let mut checksums = None;
    for a in &rel.assets {
        if a.name == asset_name {
            download = a.browser_download_url.clone();
        } else if a.name == "checksums.txt" {
            checksums = Some(a.browser_download_url.clone());
        }
    }
    if download.is_empty() {
        download = format!(
            "https://github.com/{repo}/releases/download/{}/{asset_name}",
            rel.tag_name
        );
    }
    (download, checksums)
}

/// Copy `src` onto `dst` via a temp file in the target directory, the
/// cross-device fallback for an atomic rename. Ports Go `copyOrMove`.
pub fn copy_or_move(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    let dir = dst
        .parent()
        .ok_or_else(|| format!("no parent for {}", dst.display()))?;
    let tmp = dir.join(format!(".px0-replace-{}", std::process::id()));
    std::fs::copy(src, &tmp).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Implement `px0 --update`. Ports Go `runSelfUpdate`: exact-match
/// asset, mandatory `checksums.txt`, staged download beside the running
/// binary, `chmod +x`, a `-version` smoke run, then the atomic swap
/// (Windows: `.old` shuffle). Progress lines go to `narrate`.
pub fn run_self_update(current_ver: &str, narrate: &dyn Fn(&str)) -> Result<(), String> {
    run_self_update_at(
        current_ver,
        narrate,
        &std::env::current_exe().map_err(|e| e.to_string())?,
    )
}

/// `run_self_update` with the binary path injected, so tests can stage
/// the swap into a temp dir instead of touching the test binary.
pub fn run_self_update_at(
    current_ver: &str,
    narrate: &dyn Fn(&str),
    exec_path: &std::path::Path,
) -> Result<(), String> {
    let repo = repo_name();
    narrate(&format!("checking for updates from {repo}..."));
    let rel =
        fetch_latest_release(&repo).map_err(|e| format!("failed to fetch latest release: {e}"))?;
    let latest_ver = rel
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&rel.tag_name)
        .to_string();
    if compare_semver(&latest_ver, current_ver) <= 0 {
        narrate(&format!("px0 is already up to date (v{current_ver})"));
        return Ok(());
    }
    narrate(&format!(
        "found newer version v{latest_ver} (current: v{current_ver})"
    ));
    let asset_name = expected_asset_name(&latest_ver);
    let (download_url, checksum_url) = asset_download_url(&rel, &repo, &asset_name);
    let checksum_url = checksum_url
        .ok_or_else(|| format!("release {} does not include checksums.txt", rel.tag_name))?;

    let exec_path = resolve_symlinks(exec_path);
    narrate(&format!("downloading {asset_name}..."));
    let dir = exec_path
        .parent()
        .ok_or_else(|| "no parent for executable".to_string())?;
    let tmp_path = dir.join(format!(".px0-update-{}", std::process::id()));
    let data = download_verified_asset(&download_url, &checksum_url, &asset_name)
        .map_err(|e| format!("verify downloaded update: {e}"))?;
    // A permission failure beside the binary mirrors Go's sudo hint.
    if let Err(e) = std::fs::write(&tmp_path, &data) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            return Err(format!(
                "permission denied writing to {}. Try running with 'sudo px0 --update'",
                dir.display()
            ));
        }
        // Temp-dir fallback, as in Go.
        let fallback = std::env::temp_dir().join(format!(".px0-update-{}", std::process::id()));
        std::fs::write(&fallback, &data)
            .map_err(|e| format!("could not create temporary file: {e}"))?;
        std::fs::rename(&fallback, &tmp_path)
            .map_err(|e| format!("could not stage update: {e}"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed setting executable permissions: {e}"))?;
    }
    // Smoke-run the new binary before swapping it in.
    let out = std::process::Command::new(&tmp_path)
        .arg("-version")
        .output()
        .map_err(|e| format!("verification of new binary failed: {e}"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Err(format!(
            "verification of new binary failed (output: {text})"
        ));
    }
    if cfg!(windows) {
        let old_path = exec_path.with_extension("old");
        let _ = std::fs::remove_file(&old_path);
        std::fs::rename(&exec_path, &old_path)
            .map_err(|e| format!("failed to move current binary on Windows: {e}"))?;
        if let Err(e) = copy_or_move(&tmp_path, &exec_path) {
            let _ = std::fs::rename(&old_path, &exec_path);
            return Err(format!("failed to place new binary: {e}"));
        }
    } else if std::fs::rename(&tmp_path, &exec_path).is_err() {
        copy_or_move(&tmp_path, &exec_path).map_err(|e| {
            if e.contains("permission") || e.contains("Permission") {
                format!(
                    "permission denied replacing {}. Try running with 'sudo px0 --update'",
                    exec_path.display()
                )
            } else {
                format!("failed to replace binary {}: {e}", exec_path.display())
            }
        })?;
    }
    write_update_state(&UpdateState {
        last_checked: crate::telemetry::rfc3339_now(),
        latest_ver: latest_ver.clone(),
    });
    narrate(&format!(
        "px0 successfully updated to v{latest_ver} at {}",
        exec_path.display()
    ));
    Ok(())
}

fn resolve_symlinks(p: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_table_matches_go() {
        for (v1, v2, want) in [
            ("0.1.0", "0.1.0", 0),
            ("v0.1.0", "0.1.0", 0),
            ("0.1.0", "v0.1.0", 0),
            ("0.2.0", "0.1.0", 1),
            ("0.1.0", "0.2.0", -1),
            ("1.0.0", "0.9.9", 1),
            ("0.1.1", "0.1.0", 1),
            ("0.10.0", "0.9.0", 1),
            ("0.1.0-alpha", "0.1.0", 0),
            ("0.2.0", "0.1.99", 1),
        ] {
            assert_eq!(compare_semver(v1, v2), want, "compare {v1} vs {v2}");
        }
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn asset_name_uses_go_vocabulary() {
        let name = expected_asset_name("0.2.0");
        assert_eq!(
            name,
            format!(
                "px0-0.2.0-{}-{}{}",
                go_os(),
                go_arch(),
                if cfg!(windows) { ".exe" } else { "" }
            )
        );
        assert!(name.starts_with("px0-0.2.0-"));
    }

    #[test]
    fn state_round_trips_through_xdg() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let dir = crate::testutil::tempdir("upd-state");
        let _g = crate::testutil::set_env(&[("XDG_STATE_HOME", dir.path().to_str().unwrap())]);
        assert!(std::fs::metadata(dir.path().join("px0").join("update_check.json")).is_err());
        write_update_state(&UpdateState {
            last_checked: "2026-09-17T00:00:00Z".to_string(),
            latest_ver: "0.2.0".to_string(),
        });
        let back = read_update_state().expect("state must read back");
        assert_eq!(back.latest_ver, "0.2.0");
        assert_eq!(back.last_checked, "2026-09-17T00:00:00Z");
    }

    /// Ports Go `TestFetchLatestRelease` (and the fetch half of the
    /// mock self-update test): the release record and its assets decode.
    #[test]
    fn fetches_release_from_stub() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let asset = expected_asset_name("0.2.0");
        let body = serde_json::json!({
            "tag_name": "v0.2.0",
            "name": "px0 v0.2.0",
            "assets": [{"name": asset, "browser_download_url": "https://example.com/download/px0"}],
        });
        let payload = serde_json::to_vec(&body).unwrap();
        let url = crate::testutil::stub_server(
            move |_path, _body| (200, "application/json", payload.clone()),
            4,
        );
        let _g = crate::testutil::set_env(&[("PX0_UPDATE_URL", &url)]);
        let rel = fetch_latest_release("test/repo").expect("fetch must succeed");
        assert_eq!(rel.tag_name, "v0.2.0");
        assert_eq!(rel.assets.len(), 1);
        assert_eq!(rel.assets[0].name, asset);
        let (dl, _) = asset_download_url(&rel, "test/repo", &asset);
        assert_eq!(dl, "https://example.com/download/px0");
    }

    /// Ports Go `TestDownloadVerifiedAsset` plus the mismatch rejection.
    #[test]
    fn verified_download_accepts_and_rejects() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let asset = "px0-0.2.0-linux-amd64".to_string();
        let binary = b"test binary".to_vec();
        let digest = sha256_hex(&binary);
        let serve_asset = asset.clone();
        let url = crate::testutil::stub_server(
            move |path, _body| {
                if path == format!("/{serve_asset}") {
                    (200, "application/octet-stream", binary.clone())
                } else if path == "/checksums.txt" {
                    (
                        200,
                        "text/plain",
                        format!("{digest}  {serve_asset}\n").into_bytes(),
                    )
                } else {
                    (404, "text/plain", b"nope".to_vec())
                }
            },
            8,
        );
        let got = download_verified_asset(
            &format!("{url}/{asset}"),
            &format!("{url}/checksums.txt"),
            &asset,
        )
        .expect("verified download must succeed");
        assert_eq!(got, b"test binary");

        let bad_asset = asset.clone();
        let bad = crate::testutil::stub_server(
            move |path, _body| {
                if path == "/checksums.txt" {
                    (
                        200,
                        "text/plain",
                        format!("{:064x}  {bad_asset}\n", 0).into_bytes(),
                    )
                } else {
                    (200, "application/octet-stream", b"tampered binary".to_vec())
                }
            },
            8,
        );
        let err = download_verified_asset(
            &format!("{bad}/{asset}"),
            &format!("{bad}/checksums.txt"),
            &asset,
        )
        .expect_err("tampered binary must fail");
        assert!(err.contains("checksum mismatch"), "got: {err}");
    }

    #[test]
    fn checksum_rejects_missing_and_malformed() {
        let asset = "px0-0.2.0-linux-amd64";
        assert!(checksum_for(format!("{:064x}  other-asset\n", 0).as_bytes(), asset).is_err());
        assert!(checksum_for(format!("not-a-sha256  {asset}\n").as_bytes(), asset).is_err());
        let good = format!("{:064x}  *./{asset}\n", 0xa5a5);
        // `*`/`./` prefixes strip, but the digest must be 64 hex chars.
        assert!(checksum_for(good.as_bytes(), asset).is_ok());
    }
}
