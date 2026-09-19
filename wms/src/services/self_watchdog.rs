//! Self-watchdog: a dedicated OS thread (NOT a tokio task) that probes our own
//! HTTP liveness from OUTSIDE the async runtime and `abort()`s the process if the
//! runtime goes unresponsive, so systemd's `Restart=` brings the node back.
//!
//! Why an OS thread and a raw `TcpStream` (no reqwest, no tokio): the failure we
//! are guarding against is a runtime-wide stall — every tokio worker thread
//! blocked/parked, a lock everything touches deadlocked, or the process
//! swap-thrashing to a standstill (kiosk incident 2026-09-03: after a self-update
//! restart the WMS went silent for 23 min — no heartbeats, HTTP accept backlog
//! full, process in S state at 0% CPU, systemd later reported a 2.7G memory peak
//! and 955M swap peak on a 3.4 GB box; a manual `systemctl restart` fixed it). A
//! watchdog living on the runtime would be wedged by the very thing it must
//! detect. A plain OS thread with a blocking socket keeps ticking regardless.
//!
//! The probe target is `/E/health`, which is a static handler with no DB and no
//! shared state (see `main.rs::health_check`) — so it answers 200 as long as the
//! axum accept loop and at least one worker thread are alive. It cannot be held
//! up by a DB transaction/lock, which is exactly the property we want from a
//! liveness probe. (If `/E/health` ever grows a DB dependency, add a DB-free
//! `/E/health/live` and point `ECK_SELF_WATCHDOG_PATH` at it.)
//!
//! Env:
//! - `ECK_SELF_WATCHDOG_INTERVAL_SECS` (default 30; `0` disables the watchdog).
//! - `ECK_SELF_WATCHDOG_FAILS` (default 3) consecutive failures before abort.
//! - `ECK_SELF_WATCHDOG_GRACE_SECS` (default 60) quiet period after start before
//!   the first probe (boot work + model loads must settle first).
//! - `ECK_SELF_WATCHDOG_PATH` (default `/E/health`).
//! - `ECK_SELF_WATCHDOG_PROBE_PORT` (debug/test only) — probe THIS port instead of
//!   the real one, to force failures and verify the abort path.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tracing::{error, info, warn};

/// Spawn the watchdog OS thread. `port` is the real HTTP listen port; `handle` is
/// a clone of the running tokio runtime handle (used only to read stable runtime
/// metrics — the thread itself never enters the runtime). No-op (and logs why) if
/// disabled via `ECK_SELF_WATCHDOG_INTERVAL_SECS=0`.
pub fn spawn(port: u16, handle: Handle) {
    let interval_secs = env_u64("ECK_SELF_WATCHDOG_INTERVAL_SECS", 30);
    if interval_secs == 0 {
        info!("[self-watchdog] disabled (ECK_SELF_WATCHDOG_INTERVAL_SECS=0)");
        return;
    }
    let max_fails = env_u64("ECK_SELF_WATCHDOG_FAILS", 3).max(1);
    let grace_secs = env_u64("ECK_SELF_WATCHDOG_GRACE_SECS", 60);
    let path = std::env::var("ECK_SELF_WATCHDOG_PATH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/E/health".to_string());
    // Debug knob: probe a deliberately wrong port to exercise the abort path.
    let probe_port = std::env::var("ECK_SELF_WATCHDOG_PROBE_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(port);

    let interval = Duration::from_secs(interval_secs);
    // INFO heartbeat cadence: roughly hourly, but never less often than one probe.
    let hb_every = (3600 / interval_secs.max(1)).max(1);

    let builder = std::thread::Builder::new().name("self-watchdog".into());
    let spawned = builder.spawn(move || {
        info!(
            "[self-watchdog] armed: probe 127.0.0.1:{probe_port}{path} every {interval_secs}s, \
             abort after {max_fails} consecutive failures (grace {grace_secs}s)"
        );
        std::thread::sleep(Duration::from_secs(grace_secs));

        let mut consecutive_fails: u64 = 0;
        let mut first_fail_at: Option<Instant> = None;
        let mut ticks: u64 = 0;
        loop {
            let ok = probe(probe_port, &path);
            ticks += 1;
            if ok {
                let recovered = consecutive_fails > 0;
                consecutive_fails = 0;
                first_fail_at = None;
                // One line on the first green probe after grace (startup
                // confirmation) or right after a recovery, then a cheap hourly
                // baseline so journals show the runtime shape on a healthy node.
                if ticks == 1 || recovered || ticks % hb_every == 0 {
                    // Include the process memory line so the fleet journals carry
                    // an ~hourly anon/file/swap trend — the kiosk stall (2026-09-03)
                    // was an anon-RSS + swap climb that left no journal trace.
                    info!(
                        "[self-watchdog] ok — {} -- {}",
                        metrics_line(&handle),
                        proc_line()
                    );
                }
            } else {
                consecutive_fails += 1;
                let since = first_fail_at.get_or_insert_with(Instant::now);
                let unresponsive_for = since.elapsed().as_secs();
                warn!(
                    "[self-watchdog] probe {consecutive_fails}/{max_fails} FAILED \
                     (unresponsive ~{unresponsive_for}s)"
                );
                if consecutive_fails >= max_fails {
                    error!(
                        "[self-watchdog] HTTP unresponsive for {unresponsive_for}s — \
                         dumping runtime state and aborting -- {} -- {}",
                        metrics_line(&handle),
                        proc_line()
                    );
                    // Give tracing a moment to flush the ERROR line to the journal
                    // before we hard-kill the process.
                    std::thread::sleep(Duration::from_millis(250));
                    std::process::abort();
                }
            }
            std::thread::sleep(interval);
        }
    });
    if let Err(e) = spawned {
        warn!("[self-watchdog] could not spawn thread: {e} — running WITHOUT a watchdog");
    }
}

/// Blocking HTTP/1.1 GET to `127.0.0.1:port{path}` with a hard 5 s budget across
/// connect + write + read. Returns true iff the status line is `HTTP/1.x 2xx`.
fn probe(port: u16, path: &str) -> bool {
    let deadline = Duration::from_secs(5);
    let addr = match ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
    {
        Some(a) => a,
        None => return false,
    };
    let mut stream = match TcpStream::connect_timeout(&addr, deadline) {
        Ok(s) => s,
        Err(_) => return false,
    };
    if stream.set_read_timeout(Some(deadline)).is_err()
        || stream.set_write_timeout(Some(deadline)).is_err()
    {
        return false;
    }
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUser-Agent: eck-self-watchdog\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    // We only need the status line; read a small prefix.
    let mut buf = [0u8; 128];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return false,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let mut parts = head.split_whitespace();
    matches!((parts.next(), parts.next()), (Some(v), Some(code))
        if v.starts_with("HTTP/") && code.starts_with('2'))
}

/// Stable tokio runtime metrics (no `tokio_unstable` required on tokio 1.53).
/// `num_blocking_threads`/`num_idle_blocking_threads` are intentionally omitted:
/// they are gated behind the `tokio_unstable` cfg, which we do not set fleet-wide.
fn metrics_line(handle: &Handle) -> String {
    let m = handle.metrics();
    format!(
        "rt.workers={} rt.alive_tasks={} rt.global_queue_depth={}",
        m.num_workers(),
        m.num_alive_tasks(),
        m.global_queue_depth(),
    )
}

/// Thread count + memory breakdown from `/proc/self/status` (Linux). Beyond
/// total `VmRSS` we surface `RssAnon` (owned heap — the on-device model copies
/// live here), `RssFile` (mmapped weights / binary — reclaimable), and `VmSwap`
/// (paged out — the swap-thrash signature). Empty-ish on other OSes.
fn proc_line() -> String {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string("/proc/self/status") {
            Ok(s) => {
                let mut threads = "?";
                let mut rss = "?";
                let mut anon = "?";
                let mut file = "?";
                let mut swap = "?";
                for line in s.lines() {
                    if let Some(v) = line.strip_prefix("Threads:") {
                        threads = v.trim();
                    } else if let Some(v) = line.strip_prefix("VmRSS:") {
                        rss = v.trim();
                    } else if let Some(v) = line.strip_prefix("RssAnon:") {
                        anon = v.trim();
                    } else if let Some(v) = line.strip_prefix("RssFile:") {
                        file = v.trim();
                    } else if let Some(v) = line.strip_prefix("VmSwap:") {
                        swap = v.trim();
                    }
                }
                format!(
                    "proc.threads={threads} proc.rss={rss} proc.anon={anon} \
                     proc.file={file} proc.swap={swap}"
                )
            }
            Err(e) => format!("proc.status_unavailable={e}"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        "proc.threads=n/a proc.mem=n/a (non-linux)".to_string()
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_wrong_port_fails_fast() {
        // Nothing is listening on 1 in a test process → connect fails quickly,
        // well inside the 5 s budget, and the probe reports unhealthy.
        let start = Instant::now();
        assert!(!probe(1, "/E/health"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn probe_ok_against_a_204_server() {
        // Stand up a throwaway blocking TCP server that answers 200, and confirm
        // the probe parses the status line as healthy.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut scratch = [0u8; 512];
                let _ = sock.read(&mut scratch);
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        assert!(probe(port, "/E/health"));
        let _ = h.join();
    }

    #[test]
    fn env_u64_parses_and_defaults() {
        assert_eq!(env_u64("ECK_SELF_WATCHDOG__NOPE__", 7), 7);
    }
}
