//! Data-at-rest: the whole `data/` directory as an encrypted gocryptfs volume
//! that the wms process mounts for itself at boot (`.eck/DATA_AT_REST.md`).
//!
//! Owner decision 2026-09-11: *root on the kiosk may move / delete / back up WMS
//! data, but must not be able to read it.* So the plaintext `data/` tree stops
//! existing on disk: the bytes live in `ECK_DATAFS_CIPHER_DIR` as gocryptfs
//! ciphertext, and wms mounts them at `ECK_DATAFS_MOUNT` (default `data`) using
//! a passphrase derived from the **vault** — `HKDF(TMK, "datafs", epoch)`, which
//! on a safe-sourced node exists only in RAM and is rebuilt at every boot from
//! two Shamir shares on eck1/2/3.
//!
//! Threat-model delta vs `.eck/PII_VAULT.md`:
//! * **T1 / T3 (stolen disk or backup)** — closed for *everything* in `data/`,
//!   not just the vault-tokenised columns: SurrealKV files, the CAS filestore,
//!   logs, backups, `secrets.env`. Nothing on the disk opens it.
//! * **NEW: local root on a running node** — FUSE's default (no `allow_other`,
//!   and `/etc/fuse.conf` deliberately without `user_allow_other`) means the
//!   live mount is readable *only by the mounting uid*. `root` gets `EACCES` on
//!   the mountpoint. This is a convenience barrier, not a crypto one: root can
//!   still `ptrace`/read `/proc/<pid>/mem` — hence `PR_SET_DUMPABLE=0` +
//!   `LimitCORE=0` in the unit drop-in. T5 (live memory dump) stays accepted.
//!
//! Everything here is **opt-in**: with `ECK_DATAFS_CIPHER_DIR` unset every entry
//! point is a no-op and the Windows dev node behaves exactly as before.
//!
//! ## Key delivery
//! The gocryptfs master key is random and lives — scrypt-wrapped — in
//! `<cipher>/gocryptfs.conf`. What we derive is the **passphrase** that unwraps
//! it: `hex(SHA256("9eck-datafs-gocryptfs-v1" ‖ DFK))`. Because DFK is a pure
//! HKDF of the TMK at a fixed epoch, re-deriving it on any later boot always
//! reopens the same volume — there is nothing extra to back up beyond what the
//! vault already protects. The passphrase reaches gocryptfs over a **pipe**
//! only: never argv, never a file, never the child's environment (the child is
//! spawned with a cleared env).
//!
//! ## Boot order (`wms/src/main.rs`)
//! `dotenv → keyring::init (safe/keyfile; needs only env outside the mount)
//!  → datafs::mount_if_configured → load <mount>/secrets.env → DB connect`.
//! Anything the mount needs — `INSTANCE_ID`, `ECK_VAULT_NODES`,
//! `ECK_LICENSE_TOKEN`, the node KEK — must therefore live **outside** the
//! volume; everything else (`SYNC_SECRET`, `MESH_DATA_KEY`, `JWT_SECRET`,
//! `ENC_KEY`, API tokens) moves into `secrets.env` inside it.
//!
//! ## Failure policy
//! Configured-but-unmountable is **fatal**: the caller logs and exits non-zero.
//! There is no silent plaintext fallback — a node that cannot reach its keys
//! must not recreate a clear `data/` under the mountpoint.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use zeroize::Zeroizing;

/// Cipher directory holding the gocryptfs ciphertext. **Unset = feature off.**
pub const ENV_CIPHER_DIR: &str = "ECK_DATAFS_CIPHER_DIR";
/// Mountpoint the plaintext view appears at. Default [`DEFAULT_MOUNT`].
pub const ENV_MOUNT: &str = "ECK_DATAFS_MOUNT";
/// `gocryptfs` binary (default: resolved on `PATH`).
pub const ENV_BIN: &str = "ECK_DATAFS_GOCRYPTFS";
/// Override for the post-mount secrets file. Default `<mount>/secrets.env`.
pub const ENV_SECRETS_FILE: &str = "ECK_SECRETS_FILE";
/// `1` → unmount the volume on graceful shutdown. Default OFF: leaving the
/// gocryptfs daemon up is what lets `Restart=always` re-attach in milliseconds
/// without another safe round-trip.
pub const ENV_UNMOUNT_ON_EXIT: &str = "ECK_DATAFS_UNMOUNT_ON_EXIT";

/// Default mountpoint — the historical plaintext data dir.
pub const DEFAULT_MOUNT: &str = "data";
/// Basename of the post-mount secrets file.
pub const SECRETS_BASENAME: &str = "secrets.env";
/// gocryptfs' own config file; its presence marks an initialised cipher dir.
const CONF_NAME: &str = "gocryptfs.conf";
/// Domain tag for the passphrase KDF. Changing it orphans every volume.
const PW_DOMAIN: &[u8] = b"9eck-datafs-gocryptfs-v1";

// ─── configuration ───────────────────────────────────────────────────────────

/// Resolved paths for an enabled volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cipher_dir: PathBuf,
    pub mount: PathBuf,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// The configured volume, or `None` when `ECK_DATAFS_CIPHER_DIR` is unset
/// (feature off — every other entry point no-ops).
pub fn config() -> Option<Config> {
    let cipher_dir = env_path(ENV_CIPHER_DIR)?;
    Some(Config {
        cipher_dir,
        mount: mount_dir(),
    })
}

/// The data mountpoint — `ECK_DATAFS_MOUNT`, else `data`. Defined even when the
/// feature is off, because [`secrets_path`] uses it either way.
pub fn mount_dir() -> PathBuf {
    env_path(ENV_MOUNT).unwrap_or_else(|| PathBuf::from(DEFAULT_MOUNT))
}

/// Is the encrypted data volume configured on this node?
pub fn is_configured() -> bool {
    config().is_some()
}

// ─── /proc/self/mounts ───────────────────────────────────────────────────────

/// What the kernel says about our mountpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountState {
    /// Nothing (of ours) is mounted there.
    Absent,
    /// A live `fuse.gocryptfs` mount of exactly our cipher dir — reuse it.
    Ours,
    /// A `fuse.gocryptfs` mount of a *different* source. Never silently reuse:
    /// it would hand a foreign volume to the DB.
    Foreign(String),
}

/// Un-escape the octal escapes `/proc/*/mounts` uses for space, tab, newline
/// and backslash. (`/home/a\040b` → `/home/a b`.)
fn unescape_mount_field(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            let oct = &s[i + 1..i + 4];
            if oct.bytes().all(|c| (b'0'..=b'7').contains(&c)) {
                if let Ok(v) = u8::from_str_radix(oct, 8) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pure parser over `/proc/self/mounts` content — the testable core of
/// [`mount_state`]. `mountpoint` and `cipher_dir` must already be canonical
/// (or at least spelled the same way the mount was made).
///
/// The **last** matching line wins: mounts stack, and the top of the stack is
/// what a path lookup actually reaches.
pub fn parse_mount_state(mounts: &str, mountpoint: &str, cipher_dir: &str) -> MountState {
    let mut state = MountState::Absent;
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(src), Some(mp), Some(fstype)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if unescape_mount_field(mp) != mountpoint {
            continue;
        }
        if !fstype.starts_with("fuse.gocryptfs") {
            // Some other fs mounted here (or a bind of it) — ignore; it is not
            // one of ours, and `Absent` makes us try to mount (which will fail
            // loudly rather than silently using the wrong bytes).
            continue;
        }
        let src = unescape_mount_field(src);
        state = if src == cipher_dir {
            MountState::Ours
        } else {
            MountState::Foreign(src)
        };
    }
    state
}

/// `/proc/self/mounts`, or an empty string where the file does not exist
/// (Windows / macOS → the feature is Linux-only anyway).
fn read_mounts() -> String {
    std::fs::read_to_string("/proc/self/mounts").unwrap_or_default()
}

fn canon(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

/// Live mount state of the configured volume ([`MountState::Absent`] when the
/// feature is off).
pub fn mount_state() -> MountState {
    let Some(cfg) = config() else {
        return MountState::Absent;
    };
    mount_state_of(&cfg.cipher_dir, &cfg.mount)
}

/// Live mount state of an arbitrary cipher-dir/mountpoint pair (the CLI works on
/// explicit paths that need not be the configured ones).
pub fn mount_state_of(cipher_dir: &Path, mount: &Path) -> MountState {
    parse_mount_state(&read_mounts(), &canon(mount), &canon(cipher_dir))
}

/// Is the configured volume mounted right now?
pub fn is_mounted() -> bool {
    mount_state() == MountState::Ours
}

// ─── passphrase ──────────────────────────────────────────────────────────────

/// `hex(SHA256("9eck-datafs-gocryptfs-v1" ‖ dfk))` — the string handed to
/// gocryptfs. Deterministic, so a re-derived DFK always reopens the volume; a
/// one-way function of the DFK, so a gocryptfs bug can never leak the vault key
/// itself. 64 ASCII chars: no newline / encoding hazards on the pipe.
pub fn passphrase(dfk: &[u8; 32]) -> Zeroizing<String> {
    let mut h = Sha256::new();
    h.update(PW_DOMAIN);
    h.update(dfk);
    let d: [u8; 32] = h.finalize().into();
    Zeroizing::new(hex::encode(d))
}

/// The DFK for this node, or a human-actionable error.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn require_key() -> Result<Zeroizing<[u8; 32]>, String> {
    crate::keyring::datafs_key().map(Zeroizing::new).ok_or_else(|| {
        format!(
            "no data-at-rest key available (keyring source = {}). The encrypted data \
             volume needs a vault-sourced key: enrol this node in the safe \
             (ECK_VAULT_NODES + ECK_VAULT_ENROLL_CODE) or, on a self-hosted box, point \
             ECK_DATAFS_KEYFILE at a key stored on a DIFFERENT medium than {}.",
            crate::keyring::source().as_str(),
            ENV_CIPHER_DIR
        )
    })
}

// ─── running gocryptfs ───────────────────────────────────────────────────────

fn gocryptfs_bin() -> String {
    std::env::var(ENV_BIN)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "gocryptfs".to_string())
}

/// Spawn gocryptfs with `args`, feeding `pw` over stdin.
///
/// Two passphrase-delivery shapes are tried, because which one a build accepts
/// has moved between gocryptfs releases; both keep the secret on a pipe:
/// 1. `-passfile /dev/stdin` (the documented scripted form, 2.x);
/// 2. bare stdin (gocryptfs reads the password from a non-tty stdin).
///
/// The child is spawned with a **cleared environment** (only `PATH`, `HOME`,
/// `TMPDIR`) so no wms secret is visible in `/proc/<gocryptfs>/environ`.
///
/// gocryptfs daemonises on a successful mount: the parent we wait on exits 0
/// *after* the mount is live, and the daemon `dup2`s its std fds to `/dev/null`
/// before backgrounding, so `wait_with_output` cannot hang on an inherited pipe.
fn run_gocryptfs(args: &[&str], pw: &str) -> Result<String, String> {
    use std::io::Write;
    let bin = gocryptfs_bin();
    let mut last = String::new();
    for attempt in 0..2 {
        let mut cmd = Command::new(&bin);
        cmd.env_clear();
        cmd.env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
        );
        if let Ok(h) = std::env::var("HOME") {
            cmd.env("HOME", h);
        }
        if let Ok(t) = std::env::var("TMPDIR") {
            cmd.env("TMPDIR", t);
        }
        if attempt == 0 {
            cmd.args(["-passfile", "/dev/stdin"]);
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {bin}: {e} (is gocryptfs installed?)"))?;
        {
            let mut sin = child
                .stdin
                .take()
                .ok_or_else(|| "gocryptfs stdin unavailable".to_string())?;
            // Ignore a broken pipe: gocryptfs may have already bailed out, in
            // which case the exit status below is the useful error.
            let _ = sin.write_all(pw.as_bytes());
            let _ = sin.write_all(b"\n");
            let _ = sin.flush();
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("wait for {bin}: {e}"))?;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            return Ok(if stderr.is_empty() { stdout } else { stderr });
        }
        last = format!(
            "{bin} exited {} — {}",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            if stderr.is_empty() { stdout } else { stderr }
        );
        // Only the flag-shape guess is worth retrying; a wrong passphrase or a
        // missing /dev/fuse will fail identically the second time, so bail out
        // unless the failure looks like the flag was not understood.
        let looks_like_flag_problem = last.contains("passfile")
            || last.contains("flag provided but not defined")
            || last.contains("/dev/stdin");
        if attempt == 1 || !looks_like_flag_problem {
            return Err(last);
        }
        warn!("gocryptfs -passfile /dev/stdin rejected ({last}); retrying with bare stdin");
    }
    Err(last)
}

// ─── init / mount / unmount ──────────────────────────────────────────────────

/// `gocryptfs -init` the cipher dir (creating it 0700) if it has no
/// `gocryptfs.conf` yet. Returns `true` when it initialised one.
///
/// The random master key gocryptfs generates stays inside `gocryptfs.conf`,
/// scrypt-wrapped under [`passphrase`] — i.e. under the vault. There is no
/// separate secret for an operator to lose.
pub fn init_if_needed(cipher_dir: &Path, dfk: &[u8; 32]) -> Result<bool, String> {
    if cipher_dir.join(CONF_NAME).exists() {
        return Ok(false);
    }
    create_dir_700(cipher_dir)?;
    let non_empty = std::fs::read_dir(cipher_dir)
        .map_err(|e| format!("read {}: {e}", cipher_dir.display()))?
        .next()
        .is_some();
    if non_empty {
        return Err(format!(
            "{} has no {CONF_NAME} but is not empty — refusing to -init over existing files",
            cipher_dir.display()
        ));
    }
    let pw = passphrase(dfk);
    let cipher = cipher_dir.display().to_string();
    // `-q` suppresses gocryptfs printing the master key to stdout.
    run_gocryptfs(&["-init", "-q", "--", &cipher], &pw)?;
    info!("initialised gocryptfs volume at {cipher}");
    Ok(true)
}

/// Mount `cipher_dir` at `mount`. No `-allow_other`: the plaintext view is
/// readable **only** by the uid that mounted it (this process' uid), so `root`
/// on the box gets `EACCES` on every path under it.
pub fn mount(cipher_dir: &Path, mount: &Path, dfk: &[u8; 32]) -> Result<(), String> {
    create_dir_700(mount)?;
    let pw = passphrase(dfk);
    let c = cipher_dir.display().to_string();
    let m = mount.display().to_string();
    run_gocryptfs(&["--", &c, &m], &pw)?;
    Ok(())
}

/// `fusermount3 -u` (falling back to `fusermount -u`, then `umount`). Best
/// effort — every failure is logged, none is fatal.
pub fn unmount_best_effort(mount: &Path) {
    let m = mount.display().to_string();
    for (bin, args) in [
        ("fusermount3", vec!["-u", m.as_str()]),
        ("fusermount", vec!["-u", m.as_str()]),
        ("umount", vec![m.as_str()]),
    ] {
        match Command::new(bin).args(&args).output() {
            Ok(o) if o.status.success() => {
                info!("unmounted encrypted data volume at {m} ({bin})");
                return;
            }
            Ok(o) => warn!(
                "{bin} -u {m} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => warn!("{bin} not usable ({e})"),
        }
    }
}

/// Unmount the configured volume if `ECK_DATAFS_UNMOUNT_ON_EXIT=1`. Called from
/// the shutdown path; a no-op by default (see [`ENV_UNMOUNT_ON_EXIT`]).
pub fn unmount_on_exit_if_requested() {
    let want = std::env::var(ENV_UNMOUNT_ON_EXIT)
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !want {
        return;
    }
    if let Some(cfg) = config() {
        if is_mounted() {
            unmount_best_effort(&cfg.mount);
        }
    }
}

fn create_dir_700(p: &Path) -> Result<(), String> {
    if !p.exists() {
        std::fs::create_dir_all(p).map_err(|e| format!("create {}: {e}", p.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// The boot entry point. `Ok(false)` = feature off (nothing happened);
/// `Ok(true)` = the volume is live at the mountpoint.
///
/// **The caller must treat `Err` as fatal** and exit non-zero: continuing would
/// let SurrealDB recreate a plaintext `data/` under the mountpoint, which is
/// exactly the state this feature exists to abolish.
pub fn mount_if_configured() -> Result<bool, String> {
    let Some(cfg) = config() else {
        return Ok(false);
    };

    #[cfg(not(target_os = "linux"))]
    return Err(format!(
        "{ENV_CIPHER_DIR}={} is set, but the encrypted data volume is Linux-only \
         (gocryptfs/FUSE). Unset it on this host.",
        cfg.cipher_dir.display()
    ));

    #[cfg(target_os = "linux")]
    {
        match mount_state() {
            MountState::Ours => {
                // A previous process instance left the gocryptfs daemon running
                // (Restart=always). Re-attach: no key round-trip, no remount.
                info!(
                    "encrypted data volume already mounted at {} (reusing the live gocryptfs daemon)",
                    cfg.mount.display()
                );
                return Ok(true);
            }
            MountState::Foreign(src) => {
                return Err(format!(
                    "{} is already a gocryptfs mount of {src}, not of {} — refusing to \
                     run against a foreign volume",
                    cfg.mount.display(),
                    cfg.cipher_dir.display()
                ));
            }
            MountState::Absent => {}
        }

        let dfk = require_key()?;
        if init_if_needed(&cfg.cipher_dir, &dfk)? {
            warn!(
                "{} was empty — a FRESH encrypted volume was initialised. If you meant to \
                 adopt existing data, stop now and run `wms datafs migrate` instead.",
                cfg.cipher_dir.display()
            );
        }
        mount(&cfg.cipher_dir, &cfg.mount, &dfk)?;

        // gocryptfs' parent only exits after the mount is live, but re-read the
        // kernel's own view rather than trusting an exit code.
        for _ in 0..10 {
            match mount_state() {
                MountState::Ours => {
                    info!(
                        "encrypted data volume mounted: {} → {} (uid-private, no allow_other)",
                        cfg.cipher_dir.display(),
                        cfg.mount.display()
                    );
                    return Ok(true);
                }
                MountState::Foreign(src) => {
                    return Err(format!("mounted, but /proc/self/mounts says source is {src}"))
                }
                MountState::Absent => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
        Err(format!(
            "gocryptfs reported success but {} is not a fuse.gocryptfs mount in \
             /proc/self/mounts",
            cfg.mount.display()
        ))
    }
}

// ─── secrets.env (post-mount) ────────────────────────────────────────────────

/// Where the post-mount secrets file lives: `ECK_SECRETS_FILE`, else
/// `<mount>/secrets.env`.
pub fn secrets_path() -> PathBuf {
    env_path(ENV_SECRETS_FILE).unwrap_or_else(|| mount_dir().join(SECRETS_BASENAME))
}

/// Load `<mount>/secrets.env` into the process environment **after** the volume
/// is mounted. Variables already set (by `.env` or by systemd) are left alone —
/// `dotenvy::from_path` never overrides — so the file is purely additive and a
/// boot-critical var can still be forced from the unit.
///
/// This is where `SYNC_SECRET`, `MESH_DATA_KEY`, `ENC_KEY`, `JWT_SECRET`,
/// `ODOO_API_KEY`, the AI keys, `XELIXIR_SERVICE_TOKEN` and the `ECK_MCP_*`
/// tokens live once migrated; `.env` keeps only boot config (ports, paths,
/// `INSTANCE_ID`, `RELAY_URLS`, `ECK_VAULT_*`, `ECK_DATAFS_*`).
///
/// `Ok(None)` = no such file (the normal state on the dev node). An unreadable
/// or malformed file is an error the caller should log loudly — dotenvy stops at
/// the first bad line, so half a secrets file is worse than none.
pub fn load_secrets() -> Result<Option<PathBuf>, String> {
    let path = secrets_path();
    if !path.exists() {
        return Ok(None);
    }
    warn_if_group_readable(&path);
    dotenvy::from_path(&path).map_err(|e| format!("load {}: {e}", path.display()))?;
    Ok(Some(path))
}

#[cfg(unix)]
fn warn_if_group_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(md) = std::fs::metadata(path) {
        let mode = md.permissions().mode() & 0o077;
        if mode != 0 {
            warn!(
                "{} is mode {:o} — secrets should be 0600 (chmod 600)",
                path.display(),
                md.permissions().mode() & 0o7777
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_group_readable(_path: &Path) {}

// ─── status ──────────────────────────────────────────────────────────────────

/// Admin-facing status: `{enabled, mounted, cipher_dir, mount}`. Never any key
/// material — the passphrase is not derivable from anything here.
pub fn status_json() -> Value {
    match config() {
        Some(cfg) => json!({
            "enabled": true,
            "mounted": mount_state() == MountState::Ours,
            "cipher_dir": cfg.cipher_dir.display().to_string(),
            "mount": cfg.mount.display().to_string(),
        }),
        None => json!({ "enabled": false, "mounted": false, "cipher_dir": Value::Null }),
    }
}

/// Device-facing subset: no filesystem paths.
pub fn status_json_public() -> Value {
    json!({
        "enabled": is_configured(),
        "mounted": mount_state() == MountState::Ours,
    })
}

// ─── migration (`wms datafs migrate`) ────────────────────────────────────────

/// Outcome of a [`migrate`] run.
#[derive(Debug, Default)]
pub struct MigrateReport {
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
    /// Files whose SHA-256 was compared byte-for-byte through the volume.
    pub sampled: usize,
    pub sampled_bytes: u64,
    /// Human-readable problems; non-empty = do NOT swap the directories.
    pub problems: Vec<String>,
}

impl MigrateReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn walk(root: &Path, rel: PathBuf, out: &mut Vec<(PathBuf, bool, u64)>) -> Result<(), String> {
    let dir = root.join(&rel);
    for e in std::fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))? {
        let e = e.map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        let name = e.file_name();
        let child = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        let md = e
            .metadata()
            .map_err(|e| format!("stat {}: {e}", child.display()))?;
        if md.is_dir() {
            out.push((child.clone(), true, 0));
            walk(root, child, out)?;
        } else if md.is_file() {
            out.push((child, false, md.len()));
        }
        // Symlinks / sockets are neither counted nor verified; `cp -a` still
        // copies symlinks, and the WMS data dir has none by design.
    }
    Ok(())
}

fn sha256_file(p: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| format!("open {}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", p.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Copy the plaintext `from` tree into a freshly-initialised encrypted volume at
/// `cipher`, then verify it through the mount.
///
/// Sequence: `-init` (refusing a non-empty cipher dir) → mount at a temporary
/// mountpoint `<cipher>.migrate-mnt` → `cp -a from/. mnt/` → compare the full
/// relative-path set and every file size, SHA-256 a sample (every 50th file plus
/// the 5 largest) → unmount. **The plaintext tree is never touched or deleted** —
/// swapping the directories is a deliberate operator step afterwards.
pub fn migrate(from: &Path, cipher: &Path, dfk: &[u8; 32]) -> Result<MigrateReport, String> {
    if !from.is_dir() {
        return Err(format!("{} is not a directory", from.display()));
    }
    if from.file_name().is_none() {
        return Err("refusing to migrate a path with no basename".into());
    }
    init_if_needed(cipher, dfk)?;

    let tmp_mnt = PathBuf::from(format!("{}.migrate-mnt", cipher.display()));
    create_dir_700(&tmp_mnt)?;
    if parse_mount_state(&read_mounts(), &canon(&tmp_mnt), &canon(cipher)) == MountState::Ours {
        unmount_best_effort(&tmp_mnt);
    }
    mount(cipher, &tmp_mnt, dfk)?;

    let result = (|| -> Result<MigrateReport, String> {
        // `cp -a` preserves mode/mtime/symlinks and is measurably faster than a
        // naive Rust walk on the kiosk's 3.3 GB tree.
        let src = format!("{}/.", from.display());
        let dst = format!("{}/", tmp_mnt.display());
        let out = Command::new("cp")
            .args(["-a", "--", &src, &dst])
            .output()
            .map_err(|e| format!("spawn cp: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cp -a failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        let mut a = Vec::new();
        walk(from, PathBuf::new(), &mut a)?;
        let mut b = Vec::new();
        walk(&tmp_mnt, PathBuf::new(), &mut b)?;
        a.sort();
        b.sort();

        let mut rep = MigrateReport::default();
        for (_rel, is_dir, len) in &a {
            if *is_dir {
                rep.dirs += 1;
            } else {
                rep.files += 1;
                rep.bytes += *len;
            }
        }
        if a.len() != b.len() {
            rep.problems.push(format!(
                "entry count differs: source {} vs volume {}",
                a.len(),
                b.len()
            ));
        }
        let bmap: std::collections::HashMap<_, _> =
            b.iter().map(|(r, d, l)| (r.clone(), (*d, *l))).collect();
        for (rel, is_dir, len) in &a {
            match bmap.get(rel) {
                None => rep.problems.push(format!("missing in volume: {}", rel.display())),
                Some((d2, _)) if d2 != is_dir => {
                    rep.problems.push(format!("kind differs: {}", rel.display()))
                }
                Some((_, l2)) if !*is_dir && l2 != len => rep.problems.push(format!(
                    "size differs: {} ({len} vs {l2})",
                    rel.display()
                )),
                _ => {}
            }
        }

        // SHA-256 sample: every 50th file plus the 5 largest (SurrealKV files
        // and CAS blobs — where a truncated copy would hurt most).
        let mut files: Vec<&(PathBuf, bool, u64)> = a.iter().filter(|(_, d, _)| !d).collect();
        let mut sample: Vec<PathBuf> =
            files.iter().step_by(50).map(|(r, _, _)| r.clone()).collect();
        files.sort_by_key(|(_, _, l)| std::cmp::Reverse(*l));
        for (r, _, _) in files.iter().take(5) {
            if !sample.contains(r) {
                sample.push(r.clone());
            }
        }
        for rel in &sample {
            let sp = from.join(rel);
            let dp = tmp_mnt.join(rel);
            if !dp.exists() {
                continue; // already reported as missing
            }
            let (h1, h2) = (sha256_file(&sp)?, sha256_file(&dp)?);
            rep.sampled += 1;
            rep.sampled_bytes += std::fs::metadata(&sp).map(|m| m.len()).unwrap_or(0);
            if h1 != h2 {
                rep.problems
                    .push(format!("sha256 MISMATCH: {}", rel.display()));
            }
        }
        Ok(rep)
    })();

    unmount_best_effort(&tmp_mnt);
    let _ = std::fs::remove_dir(&tmp_mnt);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid 0 0
/dev/mapper/vg-root / ext4 rw,relatime 0 0
/home/dimi/9eck.com/data.enc /home/dimi/9eck.com/data fuse.gocryptfs rw,nosuid,nodev,relatime,user_id=1001,group_id=1001 0 0
tmpfs /run tmpfs rw 0 0
";

    #[test]
    fn mount_state_detects_our_live_volume() {
        assert_eq!(
            parse_mount_state(
                MOUNTS,
                "/home/dimi/9eck.com/data",
                "/home/dimi/9eck.com/data.enc"
            ),
            MountState::Ours
        );
    }

    #[test]
    fn mount_state_rejects_a_foreign_cipher_dir() {
        // Same mountpoint, someone else's ciphertext → must NOT be reused.
        assert_eq!(
            parse_mount_state(MOUNTS, "/home/dimi/9eck.com/data", "/srv/other.enc"),
            MountState::Foreign("/home/dimi/9eck.com/data.enc".into())
        );
    }

    #[test]
    fn mount_state_absent_when_unmounted_or_other_fs() {
        assert_eq!(
            parse_mount_state(MOUNTS, "/home/dimi/9eck.com/other", "/x"),
            MountState::Absent
        );
        // A non-gocryptfs fs at the mountpoint is not "ours".
        assert_eq!(parse_mount_state(MOUNTS, "/run", "/x"), MountState::Absent);
        assert_eq!(parse_mount_state("", "/a", "/b"), MountState::Absent);
    }

    #[test]
    fn mount_state_unescapes_octal_paths_and_takes_the_top_of_the_stack() {
        let m = "\
/srv/a\\040b.enc /srv/a\\040b fuse.gocryptfs rw 0 0
/srv/old.enc /srv/mnt fuse.gocryptfs rw 0 0
/srv/new.enc /srv/mnt fuse.gocryptfs rw 0 0
";
        assert_eq!(parse_mount_state(m, "/srv/a b", "/srv/a b.enc"), MountState::Ours);
        // Stacked mounts: the LAST line is what a lookup reaches.
        assert_eq!(parse_mount_state(m, "/srv/mnt", "/srv/new.enc"), MountState::Ours);
        assert_eq!(
            parse_mount_state(m, "/srv/mnt", "/srv/old.enc"),
            MountState::Foreign("/srv/new.enc".into())
        );
    }

    #[test]
    fn passphrase_is_deterministic_hides_the_key_and_separates_volumes() {
        let k = [7u8; 32];
        let pw = passphrase(&k);
        assert_eq!(*pw, *passphrase(&k), "must reopen the same volume");
        assert_eq!(pw.len(), 64, "64 hex chars");
        assert!(pw.chars().all(|c| c.is_ascii_hexdigit()));
        // One-way: the DFK is not recoverable from (and not present in) the pw.
        assert_ne!(*pw, hex::encode(k));
        assert_ne!(*pw, *passphrase(&[8u8; 32]));
    }

    #[test]
    fn config_is_off_unless_the_cipher_dir_is_set() {
        std::env::remove_var(ENV_CIPHER_DIR);
        assert!(config().is_none());
        assert!(!is_configured());
        assert_eq!(mount_state(), MountState::Absent);
        assert_eq!(status_json()["enabled"], serde_json::json!(false));
        // A blank value counts as unset (a commented-out .env line).
        std::env::set_var(ENV_CIPHER_DIR, "   ");
        assert!(config().is_none());
        std::env::remove_var(ENV_CIPHER_DIR);
    }

    #[test]
    fn secrets_path_defaults_under_the_mount() {
        std::env::remove_var(ENV_SECRETS_FILE);
        std::env::remove_var(ENV_MOUNT);
        assert_eq!(secrets_path(), PathBuf::from("data").join("secrets.env"));
        std::env::set_var(ENV_MOUNT, "/srv/data");
        assert_eq!(secrets_path(), PathBuf::from("/srv/data/secrets.env"));
        std::env::set_var(ENV_SECRETS_FILE, "/etc/9eck/s.env");
        assert_eq!(secrets_path(), PathBuf::from("/etc/9eck/s.env"));
        std::env::remove_var(ENV_SECRETS_FILE);
        std::env::remove_var(ENV_MOUNT);
    }

    #[test]
    fn load_secrets_is_additive_and_noop_when_absent() {
        let dir = std::env::temp_dir().join(format!("eck_datafs_secrets_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secrets.env");
        std::env::set_var(ENV_SECRETS_FILE, &file);

        let _ = std::fs::remove_file(&file);
        assert_eq!(load_secrets().unwrap(), None, "absent file must be ignored");

        std::env::set_var("ECK_TEST_DATAFS_PRESET", "from-env");
        std::env::remove_var("ECK_TEST_DATAFS_NEW");
        std::fs::write(
            &file,
            "ECK_TEST_DATAFS_PRESET=from-file\nECK_TEST_DATAFS_NEW=hello\n",
        )
        .unwrap();
        assert_eq!(load_secrets().unwrap(), Some(file.clone()));
        // Additive: a var already in the environment wins (boot config in the
        // unit / .env must not be silently overridden from inside the volume).
        assert_eq!(std::env::var("ECK_TEST_DATAFS_PRESET").unwrap(), "from-env");
        assert_eq!(std::env::var("ECK_TEST_DATAFS_NEW").unwrap(), "hello");

        std::env::remove_var(ENV_SECRETS_FILE);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}
