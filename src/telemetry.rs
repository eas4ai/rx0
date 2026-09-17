//! Anonymous usage telemetry to PostHog: opt-in by build, opt-out by
//! flag, environment, or settings.
//!
//! Ports `telemetry.go`: the same events and enrichment, a 128-deep
//! drop-on-full queue drained by one worker, and synchronous
//! session-end dispatch so process exit cannot lose it.

use serde_json::{Map, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const DEFAULT_POSTHOG_HOST: &str = "https://us.i.posthog.com";
const QUEUE_SIZE: usize = 128;

/// Compile-time key (`PX0_POSTHOG_KEY` env at build), overridable at
/// runtime. Ports Go's `-X main.posthogKey` ldflag.
fn api_key() -> String {
    if let Ok(k) = std::env::var("PX0_POSTHOG_KEY") {
        if !k.trim().is_empty() {
            return k.trim().to_string();
        }
    }
    option_env!("PX0_POSTHOG_KEY")
        .unwrap_or("")
        .trim()
        .to_string()
}

fn posthog_host() -> String {
    let host = std::env::var("PX0_POSTHOG_HOST").unwrap_or_default();
    let host = host.trim().trim_end_matches('/').to_string();
    if host.is_empty() {
        DEFAULT_POSTHOG_HOST.to_string()
    } else {
        host
    }
}

/// Mirrors Go `isOptedOut`: CLI flag, `DO_NOT_TRACK=1`, or
/// `PX0_TELEMETRY` in {0,false,off,no}.
pub fn is_opted_out(flag_no_telemetry: bool) -> bool {
    if flag_no_telemetry {
        return true;
    }
    if std::env::var("DO_NOT_TRACK").as_deref() == Ok("1") {
        return true;
    }
    matches!(
        std::env::var("PX0_TELEMETRY")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// File-count privacy tiers. Ports Go `filesBucket`.
pub fn files_bucket(n: usize) -> &'static str {
    if n >= 100_000 {
        ">100k"
    } else if n >= 50_000 {
        "50k-100k"
    } else if n >= 10_000 {
        "10k-50k"
    } else if n >= 2_500 {
        "2.5k-10k"
    } else if n >= 500 {
        "500-2.5k"
    } else if n >= 100 {
        "100-500"
    } else {
        "<100"
    }
}

fn random_hex(bytes: usize) -> Option<String> {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Persistent anonymous ID in `~/.px0/anonymous_id`, ephemeral on any
/// failure. Ports Go `getOrGenerateDistinctID`.
pub fn distinct_id() -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let path = std::path::Path::new(&home)
                .join(".px0")
                .join("anonymous_id");
            if let Ok(data) = std::fs::read_to_string(&path) {
                let id = data.trim().to_string();
                if id.len() >= 16 {
                    return id;
                }
            }
        }
    }
    let id = random_hex(16).unwrap_or_else(|| {
        format!(
            "px0-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    });
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let dir = std::path::Path::new(&home).join(".px0");
            if std::fs::create_dir_all(&dir).is_ok() {
                let _ = std::fs::write(dir.join("anonymous_id"), &id);
            }
        }
    }
    id
}

/// RFC 4122 v4 UUID. Ports Go `generateUUID`.
pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        return format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12],
        b[13], b[14], b[15]
    )
}

struct Telemetry {
    api_key: String,
    host: String,
    distinct_id: String,
    session_id: String,
    start: Instant,
    sender: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<Queued>>>,
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

struct Queued {
    event: String,
    props: Map<String, Value>,
}

use std::time::Instant;

fn enrich(props: &mut Map<String, Value>, distinct_id: &str, session_id: &str) {
    props.insert(
        "distinct_id".to_string(),
        Value::String(distinct_id.to_string()),
    );
    props.insert(
        "$session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    props.insert("$lib".to_string(), Value::String("px0".to_string()));
    props.insert(
        "$lib_version".to_string(),
        Value::String(crate::VERSION.to_string()),
    );
    props.insert(
        "$os".to_string(),
        Value::String(std::env::consts::OS.to_string()),
    );
    props.insert("$arch".to_string(), Value::String(rust_arch().to_string()));
}

/// `std` names the arch `x86_64`/`aarch64`; telemetry reports Go's
/// `amd64`/`arm64` vocabulary for dashboard continuity.
fn rust_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    }
}

/// UTC `time.Now().UTC().Format(time.RFC3339)` without touching the
/// index module.
pub(crate) fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Days-to-civil date (Howard Hinnant's algorithm), then clock fields.
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3_600,
        tod % 3_600 / 60,
        tod % 60
    )
}

fn send(host: &str, api_key: &str, event: &str, props: &Map<String, Value>) {
    let debug = std::env::var("PX0_TELEMETRY_DEBUG").as_deref() == Ok("1");
    let mut payload = Map::new();
    payload.insert("api_key".to_string(), Value::String(api_key.to_string()));
    payload.insert("event".to_string(), Value::String(event.to_string()));
    payload.insert("properties".to_string(), Value::Object(props.clone()));
    payload.insert("timestamp".to_string(), Value::String(rfc3339_now()));
    let req = crate::update::http_agent(4)
        .post(&format!("{host}/capture/"))
        .header("Content-Type", "application/json")
        .header("User-Agent", format!("px0/{}", crate::VERSION))
        .send_json(Value::Object(payload));
    match req {
        Ok(resp) => {
            if debug {
                eprintln!("[telemetry] sent {event} -> {host} ({})", resp.status());
            }
        }
        Err(e) => {
            if debug {
                eprintln!("[telemetry] send {event} failed: {e}");
            }
        }
    }
}

/// The process-wide service. `None` when disabled at construction.
#[derive(Clone)]
pub struct TelemetryService {
    inner: Option<Arc<Telemetry>>,
}

impl TelemetryService {
    pub fn new(flag_no_telemetry: bool) -> Self {
        let key = api_key();
        if key.is_empty() || is_opted_out(flag_no_telemetry) {
            return Self { inner: None };
        }
        let distinct = distinct_id();
        let session = new_uuid();
        let host = posthog_host();
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Queued>(QUEUE_SIZE);
        let worker_key = key.clone();
        let worker_host = host.clone();
        let worker = std::thread::spawn(move || {
            for q in receiver {
                send(&worker_host, &worker_key, &q.event, &q.props);
            }
        });
        Self {
            inner: Some(Arc::new(Telemetry {
                api_key: key,
                host,
                distinct_id: distinct,
                session_id: session,
                start: Instant::now(),
                sender: std::sync::Mutex::new(Some(sender)),
                worker: std::sync::Mutex::new(Some(worker)),
                closed: AtomicBool::new(false),
            })),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Enqueue an event; drops when full, never blocks. Ports Go `Track`.
    pub fn track(&self, event: &str, mut props: Map<String, Value>) {
        let Some(t) = self.inner.as_ref() else { return };
        enrich(&mut props, &t.distinct_id, &t.session_id);
        if let Some(sender) = t.sender.lock().unwrap().as_ref() {
            let _ = sender.try_send(Queued {
                event: event.to_string(),
                props,
            });
        }
    }

    /// Drain the queue, then emit session end synchronously. Ports Go
    /// `Close`: taking the sender closes the channel, the join waits out
    /// the worker, and the two session events below are what exit must
    /// not lose.
    pub fn close(&self, reason: &str) {
        let Some(t) = self.inner.as_ref() else { return };
        if t.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        drop(t.sender.lock().unwrap().take());
        if let Some(worker) = t.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
        let mut props = Map::new();
        enrich(&mut props, &t.distinct_id, &t.session_id);
        let secs = t.start.elapsed().as_secs() as i64;
        props.insert("duration_seconds".to_string(), Value::from(secs.max(0)));
        props.insert("exit_reason".to_string(), Value::String(reason.to_string()));
        send(&t.host, &t.api_key, "session_ended", &props);
        send(&t.host, &t.api_key, "session_stopped", &props);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_match_go_tiers() {
        assert_eq!(files_bucket(0), "<100");
        assert_eq!(files_bucket(99), "<100");
        assert_eq!(files_bucket(100), "100-500");
        assert_eq!(files_bucket(499), "100-500");
        assert_eq!(files_bucket(500), "500-2.5k");
        assert_eq!(files_bucket(2499), "500-2.5k");
        assert_eq!(files_bucket(2500), "2.5k-10k");
        assert_eq!(files_bucket(9999), "2.5k-10k");
        assert_eq!(files_bucket(10000), "10k-50k");
        assert_eq!(files_bucket(49999), "10k-50k");
        assert_eq!(files_bucket(50000), "50k-100k");
        assert_eq!(files_bucket(99999), "50k-100k");
        assert_eq!(files_bucket(100000), ">100k");
    }

    #[test]
    fn opt_out_flags() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        assert!(is_opted_out(true));
        let _g = crate::testutil::set_env(&[("DO_NOT_TRACK", "1")]);
        assert!(is_opted_out(false));
        drop(_g);
        for val in ["0", "false", "off", "no"] {
            let _g = crate::testutil::set_env(&[("PX0_TELEMETRY", val)]);
            assert!(is_opted_out(false), "PX0_TELEMETRY={val} should opt out");
        }
        let _g = crate::testutil::unset_env(&["DO_NOT_TRACK", "PX0_TELEMETRY"]);
        assert!(!is_opted_out(false));
    }

    #[test]
    fn uuid_shape_is_v4() {
        let id = new_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn disabled_without_key() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let _g = crate::testutil::unset_env(&["PX0_POSTHOG_KEY"]);
        let tel = TelemetryService::new(false);
        assert!(!tel.enabled());
        tel.track("session_started", Map::new());
        tel.close("normal");
    }

    /// Ports Go `TestTelemetrySessionLifecycle`: one stub PostHog must
    /// receive `session_started`, `session_ended`, `session_stopped` in
    /// order, sharing one `$session_id`.
    #[test]
    fn session_lifecycle_hits_stub() {
        let _lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let home = crate::testutil::tempdir("tel-home");
        let received: std::sync::Arc<std::sync::Mutex<Vec<Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = received.clone();
        let url = crate::testutil::stub_server(
            move |path, body| {
                assert_eq!(path, "/capture/");
                if let Ok(payload) = serde_json::from_slice::<Value>(body) {
                    seen.lock().unwrap().push(payload);
                }
                (200, "application/json", br#"{"status":"ok"}"#.to_vec())
            },
            8,
        );
        let _g = crate::testutil::set_env(&[
            ("PX0_POSTHOG_KEY", "phc_test_key_xyz"),
            ("PX0_POSTHOG_HOST", &url),
            ("HOME", home.path().to_str().unwrap()),
        ]);

        let tel = TelemetryService::new(false);
        assert!(tel.enabled());
        let mut props = Map::new();
        props.insert(
            "files_bucket".to_string(),
            Value::String("100-1k".to_string()),
        );
        props.insert("has_git".to_string(), Value::Bool(true));
        props.insert("has_lsp".to_string(), Value::Bool(false));
        tel.track("session_started", props);
        std::thread::sleep(std::time::Duration::from_millis(10));
        tel.close("normal");

        let received = received.lock().unwrap();
        assert_eq!(
            received.len(),
            3,
            "want started+ended+stopped, got {received:?}"
        );
        assert_eq!(received[0]["event"], "session_started");
        assert_eq!(received[0]["properties"]["files_bucket"], "100-1k");
        assert!(!received[0]["properties"]["distinct_id"]
            .as_str()
            .unwrap_or("")
            .is_empty());
        assert_eq!(received[1]["event"], "session_ended");
        assert_eq!(received[1]["properties"]["exit_reason"], "normal");
        assert!(received[1]["properties"].get("duration_seconds").is_some());
        assert_eq!(received[2]["event"], "session_stopped");
        assert_eq!(received[2]["properties"]["exit_reason"], "normal");
        assert!(received[2]["properties"].get("duration_seconds").is_some());
        let sid = |i: usize| {
            received[i]["properties"]["$session_id"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        assert!(!sid(0).is_empty());
        assert_eq!(sid(0), sid(1));
        assert_eq!(sid(0), sid(2));
    }
}
