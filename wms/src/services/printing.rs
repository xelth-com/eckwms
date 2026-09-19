//! Physical-printer support via the host's CUPS stack (Linux) — the WMS's
//! seam onto real label/office printers.
//!
//! Deliberately dependency-free: we shell out to `lpstat`/`lp` rather than link
//! a CUPS crate, so a node with no CUPS simply reports no printers instead of
//! failing to build or run. The kiosk and servers are Linux (the real target);
//! the Windows dev box returns empty/`Err` so it never panics or hangs.

use serde::Serialize;

/// One print queue as reported by CUPS.
#[derive(Debug, Clone, Serialize)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    /// Free-form CUPS state note ("accepting jobs", etc.) when known.
    pub state: Option<String>,
}

/// Enumerate CUPS queues that accept jobs (`lpstat -a`), marking the default
/// (`lpstat -d`). A missing/erroring `lpstat` (no CUPS on this node) yields
/// `Ok(vec![])` — printing is simply unavailable, not an error. Windows returns
/// an empty list for now.
pub async fn list_printers() -> Result<Vec<PrinterInfo>, String> {
    if cfg!(target_os = "windows") {
        // TODO(win): enumerate printers via the Windows print spooler
        // (winspool / `wmic printer`) when the dev box needs real printing.
        return Ok(Vec::new());
    }

    // Default queue: `lpstat -d` → "system default destination: <name>" or
    // "no system default destination".
    let default_name: Option<String> = match tokio::process::Command::new("lpstat")
        .arg("-d")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            s.rsplit_once(':').map(|(_, n)| n.trim().to_string()).filter(|n| !n.is_empty())
        }
        // No CUPS / lpstat absent → treat as "no printers", not a hard error.
        _ => None,
    };

    // Accepting queues: `lpstat -a` → one line per queue,
    // "<name> accepting requests since ...".
    let out = match tokio::process::Command::new("lpstat").arg("-a").output().await {
        Ok(out) if out.status.success() => out,
        // lpstat missing or errored (e.g. no scheduler): no printers detected.
        _ => return Ok(Vec::new()),
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut printers = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let name = match line.split_whitespace().next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let state = line[name.len()..].trim();
        printers.push(PrinterInfo {
            is_default: default_name.as_deref() == Some(name.as_str()),
            state: (!state.is_empty()).then(|| state.to_string()),
            name,
        });
    }
    Ok(printers)
}

/// CUPS queue names are `[A-Za-z0-9_.-]`, no spaces, and never start with '-'.
fn is_valid_queue_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.len() <= 127
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Send a PDF to a CUPS queue via `lp` (bytes streamed on stdin). `printer`
/// picks the queue (`-d`); `None` uses the system default. `copies` is clamped
/// to 1..=50. Returns `lp`'s stdout (the "request id is <queue>-<job>" line).
/// Windows is unsupported for now.
pub async fn print_pdf(
    printer: Option<String>,
    pdf: Vec<u8>,
    copies: u32,
) -> Result<String, String> {
    if cfg!(target_os = "windows") {
        return Err("printing not supported on this platform yet".to_string());
    }
    if pdf.is_empty() {
        return Err("empty PDF — nothing to print".to_string());
    }

    let copies = copies.clamp(1, 50);

    let mut cmd = tokio::process::Command::new("lp");
    if let Some(ref p) = printer {
        let p = p.trim();
        if !p.is_empty() {
            // The name is user-supplied. argv is passed without a shell, so no
            // shell injection — but keep it to the CUPS queue-name charset and
            // refuse a leading '-' so it can never be read as an `lp` option.
            if !is_valid_queue_name(p) {
                return Err(format!("invalid printer name: {p:?}"));
            }
            cmd.arg("-d").arg(p);
        }
    }
    cmd.arg("-n").arg(copies.to_string());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch `lp` (is CUPS installed?): {e}"))?;

    // Stream the PDF into lp's stdin, then drop it to signal EOF.
    {
        use tokio::io::AsyncWriteExt as _;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open `lp` stdin".to_string())?;
        stdin
            .write_all(&pdf)
            .await
            .map_err(|e| format!("failed to write PDF to `lp` stdin: {e}"))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("failed to flush `lp` stdin: {e}"))?;
    }

    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("`lp` did not complete: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "`lp` failed ({}): {}",
            out.status,
            err.trim()
        ));
    }

    let job = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if job.is_empty() {
        "job submitted".to_string()
    } else {
        job
    })
}
