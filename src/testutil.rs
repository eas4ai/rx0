//! Test-only helpers: unique temp dirs, removed on drop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Unique scratch dir under the system temp dir: `px0-<tag>-<pid>-<n>`.
pub fn tempdir(tag: &str) -> TempDir {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("px0-{tag}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Serialises every test that mutates (or reads, while another mutates)
/// process-wide environment variables. Rust runs tests in threads; hold
/// this while touching `std::env`.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Saved environment restored on drop. Construct only while holding
/// [`ENV_LOCK`].
pub struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

/// Set `vars`, remembering prior values for restoration.
pub fn set_env(vars: &[(&str, &str)]) -> EnvGuard {
    let mut saved = Vec::with_capacity(vars.len());
    for (k, v) in vars {
        saved.push((k.to_string(), std::env::var(k).ok()));
        // SAFETY: caller holds ENV_LOCK, so no test thread races this.
        unsafe { std::env::set_var(k, v) };
    }
    EnvGuard { saved }
}

/// Unset `vars`, remembering prior values for restoration.
pub fn unset_env(vars: &[&str]) -> EnvGuard {
    let mut saved = Vec::with_capacity(vars.len());
    for k in vars {
        saved.push((k.to_string(), std::env::var(k).ok()));
        // SAFETY: caller holds ENV_LOCK, so no test thread races this.
        unsafe { std::env::remove_var(k) };
    }
    EnvGuard { saved }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in std::mem::take(&mut self.saved) {
            unsafe {
                match v {
                    Some(val) => std::env::set_var(&k, val),
                    None => std::env::remove_var(&k),
                }
            }
        }
    }
}

/// Minimal single-purpose HTTP server for tests: parses the request
/// line and an optional body, then answers from `handle(path, body)`.
/// Returns the base URL; the thread is detached and dies with the test
/// process. `max_conns` bounds how many requests it serves.
pub fn stub_server(
    handle: impl Fn(&str, &[u8]) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static,
    max_conns: usize,
) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::sync::Arc::new(handle);
    std::thread::spawn(move || {
        for _ in 0..max_conns {
            let (mut stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
                if buf.len() > 1 << 20 {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let want: usize = head
                .lines()
                .skip(1)
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        v.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let hlen = head.find("\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
            let mut body = if buf.len() > hlen {
                buf[hlen..].to_vec()
            } else {
                Vec::new()
            };
            while body.len() < want {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let (status, ctype, resp_body) = handle(&path, &body);
            let reason = match status {
                200 => "OK",
                404 => "Not Found",
                _ => "Error",
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                resp_body.len()
            );
            let _ = stream.write_all(&resp_body);
        }
    });
    url
}
