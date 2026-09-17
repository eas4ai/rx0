//! Live process metrics for the status bar: RSS, CPU, thread count.
//!
//! Ports `metrics.go`: Linux `/proc` reads with the same fields and the
//! 200 ms CPU resample gate. `goroutines` carries live threads, the
//! closest Rust equivalent, which is what the status bar renders.

use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ProcessMetrics {
    #[serde(rename = "rssBytes")]
    pub rss_bytes: u64,
    #[serde(rename = "cpuUsage")]
    pub cpu_usage: f64,
    pub goroutines: u64,
}

struct CpuSampler {
    last_sample: Option<Instant>,
    last_cpu_secs: f64,
    last_usage: f64,
}

static SAMPLER: Mutex<CpuSampler> = Mutex::new(CpuSampler {
    last_sample: None,
    last_cpu_secs: 0.0,
    last_usage: 0.0,
});

pub fn process_metrics() -> ProcessMetrics {
    ProcessMetrics {
        rss_bytes: read_rss(),
        cpu_usage: sample_cpu(),
        goroutines: read_threads(),
    }
}

/// Resident bytes via `/proc/self/statm`, else 0 off-Linux. Ports Go
/// `readProcessRSS` (whose MemStats fallback has no Rust equivalent).
fn read_rss() -> u64 {
    let data = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = data
        .split_whitespace()
        .nth(1)
        .and_then(|f| f.parse().ok())
        .unwrap_or(0);
    // `sysconf` exists only on Unix; off-Unix the `/proc` read above
    // already yielded zero pages, so the size is irrelevant there.
    #[cfg(unix)]
    let mut page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    #[cfg(not(unix))]
    let mut page = 4096u64;
    if page == 0 {
        page = 4096;
    }
    pages * page
}

/// Total user+system CPU seconds via `/proc/self/stat`, `None` off-Linux.
/// Field indexing mirrors Go (comm may hold spaces: cut at the last `)`).
fn read_cpu_secs() -> Option<f64> {
    let data = std::fs::read_to_string("/proc/self/stat").ok()?;
    let idx = data.rfind(')')?;
    let fields: Vec<&str> = data[idx + 2..].split_whitespace().collect();
    if fields.len() < 13 {
        return None;
    }
    let utime: f64 = fields[11].parse().ok()?;
    let stime: f64 = fields[12].parse().ok()?;
    Some((utime + stime) / 100.0)
}

fn sample_cpu() -> f64 {
    let mut s = SAMPLER.lock().unwrap();
    let now = Instant::now();
    let Some(cpu) = read_cpu_secs() else {
        return s.last_usage;
    };
    match s.last_sample {
        None => {
            s.last_sample = Some(now);
            s.last_cpu_secs = cpu;
            0.0
        }
        Some(prev) => {
            let wall = now.duration_since(prev).as_secs_f64();
            if wall >= 0.2 {
                let usage = (cpu - s.last_cpu_secs) / wall * 100.0;
                s.last_usage = usage.max(0.0);
                s.last_sample = Some(now);
                s.last_cpu_secs = cpu;
            }
            s.last_usage
        }
    }
}

/// Live thread count via `/proc/self/stat` field 20 (num_threads).
fn read_threads() -> u64 {
    let data = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let idx = match data.rfind(')') {
        Some(i) => i,
        None => return 0,
    };
    data[idx + 2..]
        .split_whitespace()
        .nth(19)
        .and_then(|f| f.parse().ok())
        .unwrap_or(0)
}
