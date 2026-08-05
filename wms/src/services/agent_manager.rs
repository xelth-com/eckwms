//! Stateful controller for the external **xelixir** C2 agent (`agent_mock`).
//!
//! xelixir provides secure, NAT-traversing remote access for maintenance/support
//! of edge/kiosk devices. The repos are kept intentionally separate; only the
//! binary name (`agent_mock`) crosses the boundary here.
//!
//! Identity is unified across both systems via the `E9_INSTANCE_ID` env var —
//! the WMS-side `instance_id` is forwarded so the xelixir agent reports the
//! same device identifier as eckWMS to the cloud.
//!
//! ## On-Demand Mesh C2 protocol
//!
//! The cloud node cannot reach the edge node directly (NAT). Commands flow via
//! the `registered_device` table, which is replicated by the P2P Merkle
//! `SyncEngine`. Every WMS instance owns a self-row at
//! `registered_device:<self_instance_id>` with `home_instance_id = self`.
//!
//! Flow:
//! 1. Cloud admin writes `xelixir_command = 'start'` on the edge's device row.
//! 2. Mesh sync propagates the row to the edge.
//! 3. The edge `AgentController` `LIVE SELECT`s its own row, sees the command.
//! 4. If `system_config:xelixir.auto_accept == true`, it spawns `agent_mock`
//!    immediately and writes `xelixir_status = 'running'` + the WS access token
//!    back to the row. Otherwise it broadcasts `XELIXIR_REQUESTED` on the WS
//!    channel and parks in `pending_approval` until a local operator hits
//!    `POST /X/approve`.
//! 5. `xelixir_command = 'stop'` kills the child and clears the token.
//!
//! `system_config:xelixir.auto_start` (the 9eck.com checkbox) controls whether
//! the controller spawns the agent at boot — and it spawns it in **STANDBY**
//! (DNO poll/dormant), never a live always-on connection. It defaults to
//! `false`: the agent starts only on a remote "Request Access" (a cloud
//! `start`), so a whole fleet doesn't even hold idle poll loops. `auto_start`
//! is mainly a first-run/provisioning convenience (a setup window is intended
//! to self-clear it). `auto_accept` defaults to `true` (a cloud `start` is
//! accepted — and brought up in standby — without local operator approval);
//! it is the client's consent to let us start the agent over the relay.
//! Live/active is only ever an on-demand wake on top of standby (or the future
//! paid self-heal mode), never these two flags.
//!
//! ## The WMS is a MANAGER, not a parent (2026-07-31)
//!
//! The agent used to be a **direct child** of the WMS, i.e. a member of the
//! `9eck-wms.service` cgroup. `systemctl restart 9eck-wms` kills the whole
//! cgroup (systemd's default `KillMode=control-group`), so the ONLY off-site
//! observability channel died at exactly the moment it mattered most — during
//! an OTA restart (see xelixir `.eck/TECH_DEBT.md`, the 2026-07-19 kiosk
//! incident: 101 rollbacks, undiagnosable through the agent).
//!
//! So on a systemd Linux host the agent is now started as a **transient
//! systemd user unit** — `systemd-run --user --unit=xelixir-agent.service` —
//! which puts it in its OWN cgroup under the session user's user manager
//! (the kiosk user has `loginctl enable-linger`). A WMS restart no longer
//! touches it; the new WMS process **re-attaches** to the still-running unit
//! (see [`AgentController::try_adopt_running_agent`]) instead of spawning a
//! second one. `setsid`/double-fork would NOT have been enough — cgroup
//! membership survives `setsid`; the process has to actually change cgroups.
//!
//! * lifecycle: `systemctl --user start/stop/restart/is-active <unit>`, never
//!   a child-process kill. Every old kill path (the `stop` command, license
//!   revocation / re-claim, WMS shutdown) goes through [`AgentController::stop_agent`],
//!   which stops the unit.
//! * token handover: the WS access token is no longer inherited through a
//!   child's environment. It is written to a `0600` env file owned by the run
//!   user under `/run/user/<uid>/` and handed to the unit via
//!   `-p EnvironmentFile=`; the token never appears on any argv (`/proc/*/cmdline`
//!   is world-readable). `stop` deletes the file.
//! * self-healing without the WMS: the unit carries `Restart=on-failure` +
//!   `RestartSec`, so the agent recovers from a crash even while the WMS is
//!   down or wedged. The WMS-side supervisor loop is only a backstop now.
//! * fallback: on Windows dev boxes, non-systemd hosts, or when `systemd-run`
//!   fails for any reason, the historical direct-child spawn is used verbatim
//!   ([`RunMode::DirectChild`]). `XELIXIR_AGENT_RUN_MODE=child|systemd` forces
//!   a mode; the default is `auto`.
//!
//! ### ⚠ Behavioural change: an agent upgrade no longer rides the WMS restart
//!
//! Because the agent is no longer a child, dropping a new `agent_mock` binary
//! in place and restarting the WMS leaves the OLD binary running. The new
//! binary is only picked up by an explicit
//! `systemctl --user restart xelixir-agent.service` — exposed here as
//! [`AgentController::restart_agent`] and reachable remotely as the
//! `restart` command (`POST /X/self/restart`, `xelixir_command = 'restart'`).
//! An OTA self-update the agent performs on itself is unaffected: it re-execs
//! inside its own unit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use surrealdb::types::SurrealValue;
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{debug, error, info, warn};
use futures_util::StreamExt;

use eck_core::db::SurrealDb;

/// Handle to a running `agent_mock` **child** — the [`RunMode::DirectChild`]
/// fallback only. Dropping the controller's `Option<AgentHandle>` triggers the
/// kill signal; the spawned wait task terminates the child and clears the DB
/// token. In [`RunMode::SystemdUser`] (the fleet default) there is no child and
/// no handle: the agent lives in its own transient unit and is stopped with
/// `systemctl --user stop`.
struct AgentHandle {
    kill: oneshot::Sender<()>,
}

/// How the WMS runs the agent process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunMode {
    /// Default on a systemd Linux host: a **transient systemd user unit**, in
    /// its own cgroup — so `systemctl restart 9eck-wms` leaves it running.
    SystemdUser,
    /// Historical behaviour: a plain child process of the WMS, which dies with
    /// the WMS cgroup. Used on Windows dev boxes, non-systemd hosts, and as the
    /// runtime fallback when `systemd-run` fails.
    DirectChild,
}

/// The OS user the agent must run as: on a Wayland kiosk it needs the
/// graphical-session user's 0700 runtime dir (screenshots + uinput), so the
/// WMS drops to that user via `sudo -u`.
#[derive(Clone, Debug)]
struct AgentUser {
    name: String,
    uid: u32,
    home: String,
}

/// Outcome of the unit-name idempotency check performed before every spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Adopt {
    /// No unit running — the caller must start one.
    NotRunning,
    /// A unit is already running and its session is reusable: re-attach to it
    /// instead of spawning a second agent (this is what makes the agent
    /// survive a WMS restart).
    Reattach { token: String, standby: bool },
    /// A unit is running but its session cannot be reused (token
    /// unrecoverable, or a different start mode was requested) — the caller
    /// must stop it and start a fresh one. Still never two live agents.
    Replace,
}

/// Fixed transient-unit name. FIXED on purpose: it is the idempotency key that
/// stops a WMS crash-restart from producing a second live agent.
const AGENT_UNIT_DEFAULT: &str = "xelixir-agent.service";

/// Hard timeout on every `systemd-run` / `systemctl` / `sudo sh` helper call —
/// the supervisor loop must never wedge on a hung subprocess.
const SYSTEMD_CMD_TIMEOUT_SECS: u64 = 10;

/// `RestartSec` of the transient unit — the agent self-heals on crash even
/// while the WMS is down. Same 5 s the WMS-side respawn used.
const UNIT_RESTART_SEC: u64 = 5;

#[derive(Clone, Debug, serde::Deserialize, SurrealValue)]
struct XelixirConfig {
    // "Auto-start agent at boot" (the 9eck.com checkbox). When true, the WMS
    // spawns the agent at startup in STANDBY (DNO poll/dormant) mode — it polls
    // xelixir every ~5 min, shows as an idle device, and goes live only on an
    // on-demand wake. NOT a live always-on connection. Normally false (mainly a
    // first-run/provisioning convenience; a future setup window will self-clear
    // it); when false the agent starts only on a remote "Request Access".
    #[serde(default = "default_false")]
    pub auto_start: bool,
    // "Auto-accept remote start requests" — the client's consent to let us start
    // the agent (in standby) on their WMS via the relay without a local operator
    // approving each session. Meaningful mainly when auto_start is off.
    #[serde(default = "default_true")]
    pub auto_accept: bool,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

/// Backoff before respawning a crashed agent when `auto_start` is on —
/// mirrors the systemd `RestartSec` the standalone unit used to provide.
/// [`RunMode::DirectChild`] only: there the WMS *is* the supervisor.
const RESPAWN_BACKOFF_SECS: u64 = 5;

/// Supervisor poll interval in [`RunMode::SystemdUser`]. Slower than the child
/// backoff on purpose: the unit's own `Restart=on-failure` already handles
/// crashes, so this loop is only a backstop for "the unit stopped entirely"
/// (and it costs a `systemctl is-active` subprocess per tick).
const UNIT_SUPERVISOR_POLL_SECS: u64 = 30;

impl Default for XelixirConfig {
    fn default() -> Self {
        Self {
            auto_start: false,
            auto_accept: true,
        }
    }
}

pub struct AgentController {
    db: SurrealDb,
    ws_tx: broadcast::Sender<String>,
    instance_id: String,
    public_key: String,
    /// Only ever `Some` in [`RunMode::DirectChild`] (or after a systemd-run
    /// failure fell back to a child). In systemd mode the agent's liveness
    /// lives in systemd, not in this process — that is the whole point.
    handle: Mutex<Option<AgentHandle>>,
    run_mode: RunMode,
    /// Transient unit name (`XELIXIR_AGENT_UNIT`, default `xelixir-agent.service`).
    unit: String,
    /// Cached graphical-session user. The /proc scan that finds it is too
    /// expensive for the supervisor tick; a *successful* lookup is cached
    /// (a failed one is not — the compositor may simply not be up yet).
    agent_user: Mutex<Option<AgentUser>>,
}

impl AgentController {
    pub fn new(
        db: SurrealDb,
        ws_tx: broadcast::Sender<String>,
        instance_id: String,
        public_key: String,
    ) -> Arc<Self> {
        let run_mode = detect_run_mode();
        let unit = normalize_unit_name(std::env::var("XELIXIR_AGENT_UNIT").ok().as_deref());
        info!(
            "[AgentController] run mode: {:?} (unit `{}`)",
            run_mode, unit
        );
        Arc::new(Self {
            db,
            ws_tx,
            instance_id,
            public_key,
            handle: Mutex::new(None),
            run_mode,
            unit,
            agent_user: Mutex::new(None),
        })
    }

    /// One-shot startup: ensure the self device row + config exist, then
    /// (if `auto_start`) spawn the agent and start the live watcher.
    pub async fn bootstrap_and_run(self: Arc<Self>) {
        if let Err(e) = self.ensure_self_device_record().await {
            warn!("[AgentController] Failed to ensure self device record: {}", e);
        }

        let cfg = self.ensure_config().await;
        info!(
            "[AgentController] config: auto_start={}, auto_accept={}",
            cfg.auto_start, cfg.auto_accept
        );

        // LICENSE_TOKEN comes from the process env, fixed at boot — without it
        // spawn_agent can NEVER succeed, so neither the boot auto-start nor the
        // 5 s supervisor loop should run (they'd warn twice every 5 s forever —
        // pure churn + log spam on unlicensed dev boxes). On-demand "Request
        // Access" still surfaces the proper error if someone tries.
        let has_license = std::env::var("LICENSE_TOKEN").map(|t| !t.is_empty()).unwrap_or(false);
        if !has_license {
            warn!(
                "[AgentController] LICENSE_TOKEN not set — xelixir agent disabled, supervisor not started (set it in .env and restart)"
            );
        }

        // RE-ATTACH FIRST. In systemd mode the agent outlives the WMS, so a
        // fresh WMS process may well find its agent already running (that IS
        // the fix — the observer no longer dies with the patient). Adopt it and
        // republish its state instead of spawning a second one, regardless of
        // `auto_start`: the running unit may have been started on-demand by a
        // relay `start` before the restart.
        let adopted = match self.try_adopt_running_agent(None).await {
            Adopt::Reattach { token, standby } => {
                info!(
                    "[AgentController] re-attached to already-running unit `{}` (mode={}) — the agent survived the WMS restart",
                    self.unit,
                    if standby { "standby" } else { "live" }
                );
                let url = session_url_for_token(&token);
                let status = if standby { "standby" } else { "running" };
                self.set_device_state(status, Some(token), Some(url)).await;
                true
            }
            Adopt::Replace => {
                warn!(
                    "[AgentController] unit `{}` is running but its access token could not be recovered — it will be replaced on the next start",
                    self.unit
                );
                false
            }
            Adopt::NotRunning => false,
        };

        // "Auto-start at boot" spawns the agent in STANDBY (DNO poll/dormant) —
        // never a live connection. Live happens only on an on-demand wake.
        if cfg.auto_start && has_license && !adopted {
            match self.spawn_agent(true).await {
                Ok(token) => {
                    info!("[AgentController] auto-started xelixir agent in STANDBY (poll) mode");
                    let url = session_url_for_token(&token);
                    self.set_device_state("standby", Some(token), Some(url)).await;
                }
                Err(e) => warn!("[AgentController] auto_start (standby) failed: {}", e),
            }
        }

        // Supervisor: when auto_start is on, keep the (standby) agent alive —
        // respawn it if it died (crash / OOM / OTA self-exec). Liveness is asked
        // of systemd (`is-active`) in unit mode and of the child handle in
        // fallback mode; we respawn HERE (not in the child's wait task — that
        // would make its future recursively contain spawn_agent). In unit mode
        // the unit's own Restart=on-failure is the first line of defence and
        // this loop only catches "the unit is gone entirely". Always respawned
        // in standby (the only flag-driven spawn mode).
        if has_license {
            let sup = Arc::clone(&self);
            let poll = if self.run_mode == RunMode::SystemdUser {
                UNIT_SUPERVISOR_POLL_SECS
            } else {
                RESPAWN_BACKOFF_SECS
            };
            tokio::spawn(async move {
                let mut was_alive = sup.agent_alive().await;
                loop {
                    tokio::time::sleep(Duration::from_secs(poll)).await;
                    let alive = sup.agent_alive().await;
                    let auto = sup.read_config().await.map(|c| c.auto_start).unwrap_or(false);
                    if !alive {
                        if auto {
                            warn!("[AgentController] supervisor: agent down + auto_start=on — respawning (standby)");
                            match sup.spawn_agent(true).await {
                                Ok(token) => {
                                    let url = session_url_for_token(&token);
                                    sup.set_device_state("standby", Some(token), Some(url)).await;
                                }
                                Err(e) => warn!("[AgentController] supervisor respawn failed: {}", e),
                            }
                        } else if was_alive {
                            // Unit mode has no child-exit callback, so the
                            // alive→dead edge is where the DB row is corrected.
                            let _ = mark_stopped_in_db(&sup.db, &sup.instance_id).await;
                        }
                    }
                    was_alive = alive;
                }
            });
        }

        // Long-running LIVE SELECT loop. On error / stream end, log and exit;
        // the parent supervisor (main.rs) is expected to restart us.
        loop {
            match self.run_live_watcher().await {
                Ok(()) => warn!("[AgentController] live watcher exited cleanly — reconnecting in 5s"),
                Err(e) => warn!("[AgentController] live watcher error: {} — reconnecting in 5s", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    /// Watch the local copy of our own device row for `xelixir_command` writes
    /// propagated in from the cloud via mesh sync.
    async fn run_live_watcher(self: &Arc<Self>) -> anyhow::Result<()> {
        info!(
            "[AgentController] LIVE SELECT registered_device WHERE home_instance_id = '{}'",
            self.instance_id
        );

        // SurrealDB LIVE SELECT does not support parameter binding on the WHERE
        // clause in all versions; inline the instance_id (it is a UUID, safe).
        let q = format!(
            "LIVE SELECT * FROM registered_device WHERE home_instance_id = '{}'",
            self.instance_id
        );
        let mut response = self.db.query(&q).await?;
        let mut stream = response.stream::<surrealdb::Notification<Value>>(0)?;

        while let Some(result) = stream.next().await {
            match result {
                Ok(notification) => {
                    let action = notification.action.to_string();
                    if action != "Create" && action != "Update" {
                        continue;
                    }
                    let row = notification.data;
                    let device_id = row
                        .get("device_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Only react to OUR self-row. Other PDA scanners that home
                    // here may share the filter; ignore them.
                    if device_id != self.instance_id {
                        continue;
                    }
                    let cmd = row
                        .get("xelixir_command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if cmd.is_empty() {
                        continue;
                    }
                    debug!(
                        "[AgentController] caught xelixir_command='{}' on self-row",
                        cmd
                    );
                    if let Err(e) = self.handle_command(&cmd).await {
                        warn!("[AgentController] handle_command({}) failed: {}", cmd, e);
                    }
                }
                Err(e) => warn!("[AgentController] live stream error: {}", e),
            }
        }

        Ok(())
    }

    /// Branch a mesh-delivered command. `auto_accept=false` parks `start` in
    /// `pending_approval` until a local operator hits `/X/approve`.
    async fn handle_command(self: &Arc<Self>, cmd: &str) -> Result<(), String> {
        match cmd {
            "start" => {
                let cfg = self.read_config().await.unwrap_or_default();
                if cfg.auto_accept {
                    self.set_device_state("starting", None, None).await;
                    // Relay-triggered start brings the agent up in STANDBY (poll);
                    // the live session is a separate on-demand wake.
                    let token = self.spawn_agent(true).await?;
                    let url = session_url_for_token(&token);
                    self.set_device_state("standby", Some(token), Some(url))
                        .await;
                } else {
                    info!("[AgentController] auto_accept=false — parking in pending_approval");
                    self.set_device_state("pending_approval", None, None).await;
                    let _ = self.ws_tx.send(
                        json!({
                            "type": "XELIXIR_REQUESTED",
                            "device_id": self.instance_id,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        })
                        .to_string(),
                    );
                }
            }
            "stop" => {
                self.stop_agent().await;
                self.set_device_state("stopped", None, None).await;
            }
            // Pick up a freshly-dropped agent binary. Needed because the agent
            // is no longer a WMS child: it does NOT ride the WMS restart any
            // more (see the module header). Keeps the same access token — the
            // unit re-reads its EnvironmentFile.
            "restart" => {
                match self.restart_agent().await {
                    Ok(()) => info!("[AgentController] agent unit restarted on request"),
                    Err(e) => warn!("[AgentController] restart failed: {}", e),
                }
            }
            other => {
                debug!("[AgentController] ignoring unknown command '{}'", other);
            }
        }
        // Clear the command so we don't re-trigger on a future sync echo.
        self.clear_device_command().await;
        Ok(())
    }

    /// Operator-initiated approval from `POST /X/approve`. Bypasses the
    /// `auto_accept` gate and brings the agent up (in STANDBY) now.
    pub async fn approve(self: &Arc<Self>) -> Result<String, String> {
        let token = self.spawn_agent(true).await?;
        let url = session_url_for_token(&token);
        self.set_device_state("standby", Some(token.clone()), Some(url))
            .await;
        self.clear_device_command().await;
        Ok(token)
    }

    /// Claim a license token at the licensing authority, then start `agent_mock`
    /// — as a transient systemd user unit when possible, else as a child.
    ///
    /// **Idempotent**: if the fixed-name unit is already running with a
    /// recoverable session, this re-attaches to it and returns its existing
    /// token instead of starting a second agent (a WMS crash-restart must never
    /// produce two live agents). Anything not adoptable is stopped first.
    ///
    /// `standby=true` forwards `XELTH_START_MODE=standby` so the agent polls
    /// xelixir until woken instead of holding a live socket — the only mode the
    /// two UI flags spawn. `standby=false` (live/always-connected) is reserved
    /// for the future WMS self-heal mode (paid), not wired to the flags.
    pub async fn spawn_agent(self: &Arc<Self>, standby: bool) -> Result<String, String> {
        // Unit-name idempotency: adopt an already-running agent in the same
        // mode rather than starting a rival one (and skip a pointless license
        // re-claim while we're at it).
        if let Adopt::Reattach { token, .. } = self.try_adopt_running_agent(Some(standby)).await {
            info!(
                "[AgentController] unit `{}` already running in the requested mode — re-attaching instead of spawning",
                self.unit
            );
            return Ok(token);
        }

        // Replace anything else that is running (stale unit, old child).
        self.stop_agent().await;

        let license_token = match std::env::var("LICENSE_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                return Err("LICENSE_TOKEN not set in .env — xelixir C2 disabled".into());
            }
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        // Default to the DIRECT rustls port :3221 — the xelixir authority is
        // reached on :3221, not via the reverse proxy on :443. The proxy's
        // `location /api/` does `return 301 …:3221`, and a 301 makes HTTP
        // clients downgrade POST→GET → the claim (a POST) comes back 405. So
        // the :443 form silently breaks license claims; mirror XELTH_WS_URL (:3221).
        let claim_url = std::env::var("XELTH_CLAIM_URL")
            .unwrap_or_else(|_| "".to_string());

        let payload = json!({
            "token": license_token,
            "instance_id": self.instance_id,
            "public_key": self.public_key,
        });

        info!("[AgentController] Claiming license at {}", claim_url);
        let res = client
            .post(&claim_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Licensing server unreachable: {}", e))?;

        let status = res.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            // Server returns 403 for both "token bound to a different device"
            // AND "token not found in the license table". The two cases want
            // very different operator responses (support ticket vs. "the
            // licensing DB is empty, you need to provision the token"), so
            // peek at the response body to disambiguate.
            let err_text = res.text().await.unwrap_or_default();
            let lower = err_text.to_lowercase();
            let unknown_token =
                lower.contains("not found") || lower.contains("unknown") || lower.contains("no such");
            let (title, msg) = if unknown_token {
                (
                    "Лицензия AI Агента не зарегистрирована",
                    "Licensing-сервер не знает этот LICENSE_TOKEN. Скорее всего запись отсутствует в таблице license на xelth.com. Это не «занятая лицензия» — это «не выдана». Обратитесь в поддержку или попросите админа создать запись.",
                )
            } else {
                (
                    "Ошибка лицензии AI Агента",
                    "Данная лицензия уже используется на другом устройстве. Если вы переносите систему на новый сервер, пожалуйста, обратитесь в службу поддержки через систему тикетов, чтобы сбросить привязку устройства.",
                )
            };
            self.broadcast_alert("critical", title, msg).await;
            return Err(format!("{} (server body: {})", msg, err_text).into());
        }
        if !status.is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(format!("Licensing server returned HTTP {}: {}", status, err_text));
        }

        let body: Value = res
            .json()
            .await
            .map_err(|e| format!("Invalid JSON from licensing server: {}", e))?;
        let ws_auth_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("No access_token in licensing response")?
            .to_string();

        // Resolve the user the agent must run AS. On a Wayland kiosk the agent
        // needs the graphical-session user's 0700 socket for screenshots +
        // uinput, so it must drop to THAT user — a sandbox uid can't reach the
        // display. `XELIXIR_AGENT_USER` overrides the auto-detected compositor
        // owner. `sudo -u <user>` re-resolves the user's groups from /etc/group
        // at spawn, so `input` (uinput access) is present without a reboot.
        // It is also the user whose `systemd --user` manager owns the transient
        // unit (that user needs `loginctl enable-linger`).
        let agent_user = self.agent_user().await;

        // Prefer the session user's own ~/bin/agent_mock (user-writable → OTA
        // self-update can swap it w/o root); else the resolved system path.
        // NOTE: path also referenced by rename plan (xelixir TECH_DEBT) — the
        // on-kiosk basename `agent_mock` stays for now even as the crate renames.
        let agent_path = agent_user
            .as_ref()
            .map(|u| PathBuf::from(&u.home).join("bin").join("agent_mock"))
            .filter(|p| p.exists())
            .or_else(resolve_agent_binary)
            .ok_or_else(|| "xelixir agent binary not found".to_string())?;

        // Where the agent dials the xelixir relay/server. Default targets port
        // 3221 — xelixir's direct rustls listener — because nginx on :443 only
        // 301-redirects /X/ws since 2026-05-26 and raw WS clients don't follow
        // redirects on the handshake. A single WMS .env controls the dial-out.
        let xelth_ws_url = std::env::var("XELTH_WS_URL")
            .unwrap_or_else(|_| "".to_string());
        let wayland_display =
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());

        // The agent's environment, built ONCE and used by both run modes: the
        // transient unit receives it as a 0600 `EnvironmentFile`, the fallback
        // child inherits it through `sudo --preserve-env`. WS_AUTH_TOKEN is the
        // secret here — it must never end up on an argv (`/proc/*/cmdline` is
        // world-readable), which is why the unit gets a file and not `--setenv`.
        let mut agent_env: Vec<(&'static str, String)> = vec![
            ("WS_AUTH_TOKEN", ws_auth_token.clone()),
            ("E9_INSTANCE_ID", self.instance_id.clone()),
            ("XELTH_WS_URL", xelth_ws_url.clone()),
        ];
        if let Some(ref u) = agent_user {
            // Wayland session env — screenshots/uinput need the session user's
            // 0700 runtime dir. Only added when we actually drop to a session
            // user, so the headless/Windows path keeps its historical env.
            agent_env.push(("XDG_RUNTIME_DIR", format!("/run/user/{}", u.uid)));
            agent_env.push(("WAYLAND_DISPLAY", wayland_display.clone()));
        }
        if standby {
            agent_env.push(("XELTH_START_MODE", "standby".to_string()));
        }

        // ── Preferred path: transient systemd user unit, i.e. its OWN cgroup ──
        // This is what makes the agent outlive `systemctl restart 9eck-wms`.
        if self.run_mode == RunMode::SystemdUser {
            match self
                .start_transient_unit(agent_user.as_ref(), &agent_path, &agent_env)
                .await
            {
                Ok(()) => {
                    info!(
                        "[AgentController] xelixir started as transient unit `{}` (bin {}, user {}, E9_INSTANCE_ID={}) — outlives WMS restarts",
                        self.unit,
                        agent_path.display(),
                        agent_user.as_ref().map(|u| u.name.as_str()).unwrap_or("<wms uid>"),
                        self.instance_id
                    );
                    return Ok(ws_auth_token);
                }
                Err(e) => {
                    // Never lose the agent over the new mechanism: degrade to
                    // the historical child spawn (which dies with the WMS) and
                    // say so loudly.
                    warn!(
                        "[AgentController] systemd-run failed ({}) — falling back to a DIRECT CHILD (it will die with the WMS)",
                        e
                    );
                    self.clear_agent_env_file(agent_user.as_ref()).await;
                }
            }
        }

        // ── Fallback path: direct child of the WMS (historical behaviour) ────
        let mut agent_cmd = if let Some(ref u) = agent_user {
            // Drop to the session user via sudo, forwarding the Wayland session
            // env so screenshots/input reach the kiosk display. WMS's uid
            // (9eckwms) needs `NOPASSWD: SETENV: (<user>) <agent_path>`.
            info!(
                "[AgentController] spawning agent as session user '{}' (uid {}), bin {}, display {}",
                u.name, u.uid, agent_path.display(), wayland_display
            );
            let mut c = Command::new("sudo");
            // sudo drops every variable that is not allow-listed, so build the
            // list from `agent_env` — the two can no longer drift apart.
            let keys: Vec<&str> = agent_env.iter().map(|(k, _)| *k).collect();
            c.arg("-n")
                .arg("-u").arg(&u.name)
                .arg(format!("--preserve-env={}", keys.join(",")))
                .arg(&agent_path);
            c
        } else {
            // No session user resolvable (headless host, or Windows): run
            // in-process as the WMS uid — historical behaviour.
            Command::new(&agent_path)
        };
        for (k, v) in &agent_env {
            agent_cmd.env(k, v);
        }
        // Give the fleet agent its OWN journal identity instead of drowning the
        // 9eck-wms unit's journal (the 95%-noise problem — xelixir TECH_DEBT):
        // route its stdout+stderr through `systemd-cat -t xelixir-agent`.
        // (Unit mode gets the same identity for free via `SyslogIdentifier=`,
        // so this side-car only exists on the child fallback path.) The
        // agent stays a DIRECT child handle (only its stdio fds are redirected),
        // so the pid tracking, respawn-on-crash, and kill paths below are
        // unchanged. Falls back to inheriting WMS's fds if systemd-cat is
        // unavailable — never fails the spawn over logging.
        let logcat: Option<Child> = spawn_agent_log_forwarder(&mut agent_cmd);
        agent_cmd.kill_on_drop(true);

        let mut child: Child = agent_cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn xelixir agent: {}", e))?;
        let pid = child.id().unwrap_or(0);
        info!(
            "[AgentController] xelixir spawned (pid={}, E9_INSTANCE_ID={})",
            pid, self.instance_id
        );

        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let db = self.db.clone();
        let iid = self.instance_id.clone();
        let ctrl = Arc::clone(self); // for crash-respawn when auto_start is on

        tokio::spawn(async move {
            tokio::select! {
                status = child.wait() => {
                    warn!("[AgentController] xelixir exited on its own: {:?}", status);
                    let _ = mark_stopped_in_db(&db, &iid).await;
                    // Mark not-running so the supervisor loop in bootstrap_and_run
                    // respawns it (in standby) when auto_start is on. Respawning
                    // inline here would make this spawned future recursively
                    // contain spawn_agent (→ infinite future type / unsatisfiable
                    // Send).
                    *ctrl.handle.lock().await = None;
                }
                _ = kill_rx => {
                    info!("[AgentController] kill signal received — terminating child");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
            // Reap the systemd-cat side-car (if any): once the agent's fds close
            // it sees EOF on stdin and exits on its own; wait() collects it so a
            // flapping agent doesn't leak a zombie per respawn.
            if let Some(mut cat) = logcat {
                let _ = cat.wait().await;
            }
        });

        *self.handle.lock().await = Some(AgentHandle { kill: kill_tx });
        Ok(ws_auth_token)
    }

    /// Stop the agent, whichever way it is running.
    ///
    /// This is the ONE kill path: the `stop` command, license revocation /
    /// re-claim (`spawn_agent` calls it before claiming), and operator stops
    /// all funnel through here. It is deliberately mode-agnostic and
    /// idempotent — it kills a leftover child AND stops the transient unit, so
    /// a WMS that fell back to a child once (or was restarted between the two)
    /// can still reliably kill whatever is actually alive.
    pub async fn stop_agent(&self) {
        // 1. Direct-child fallback: the historical oneshot kill.
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = handle.kill.send(());
        }
        // 2. Transient unit: `systemctl --user stop` tears down the unit's whole
        //    cgroup (default KillMode=control-group), so nothing of the agent
        //    tree survives. A stop on an already-dead unit is a no-op.
        if self.run_mode == RunMode::SystemdUser {
            let user = self.agent_user().await;
            match self.systemctl(user.as_ref(), &["stop"]).await {
                Ok(out) if out.status.success() => {
                    info!("[AgentController] stopped transient unit `{}`", self.unit)
                }
                Ok(out) => debug!(
                    "[AgentController] `systemctl --user stop {}` exit {:?}: {}",
                    self.unit,
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => warn!("[AgentController] could not stop unit `{}`: {}", self.unit, e),
            }
            // Clear a `failed` residue so the next `systemd-run --unit=<same>`
            // is not refused (`--collect` normally handles this; belt + braces).
            let _ = self.systemctl(user.as_ref(), &["reset-failed"]).await;
            // Token hygiene: the access token must not outlive the session.
            self.clear_agent_env_file(user.as_ref()).await;
        }
    }

    /// Restart the agent **in place**, keeping its access token (the unit
    /// re-reads the same `EnvironmentFile`).
    ///
    /// This exists because of the one behavioural change that came with
    /// demoting the WMS from parent to manager: the agent no longer dies with
    /// the WMS, so dropping a NEW `agent_mock` binary and restarting the WMS
    /// does NOT pick it up — an agent-binary upgrade needs this explicit
    /// restart (`systemctl --user restart xelixir-agent.service`).
    pub async fn restart_agent(&self) -> Result<(), String> {
        if self.run_mode != RunMode::SystemdUser {
            // Child mode: the agent already dies with the WMS, so "restart" is
            // just stop — the auto_start supervisor brings it back within
            // RESPAWN_BACKOFF_SECS (and a WMS restart picks up a new binary).
            self.stop_agent().await;
            return Ok(());
        }
        let user = self.agent_user().await;
        let out = self.systemctl(user.as_ref(), &["restart"]).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "systemctl --user restart {} exit {:?}: {}",
                self.unit,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    /// Is an agent running right now? Liveness lives in systemd in unit mode
    /// (so it is still knowable after a WMS restart) and in the child handle
    /// in fallback mode.
    pub async fn agent_alive(&self) -> bool {
        if self.handle.lock().await.is_some() {
            return true;
        }
        self.unit_active().await
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    // ─── transient-unit management ────────────────────────────────────────

    /// Cached `(username, uid, home)` of the user the agent runs as. The /proc
    /// compositor scan behind it is too heavy for the supervisor tick, so a
    /// successful lookup is cached; a failed one is not (the compositor may
    /// just not be up yet at boot).
    async fn agent_user(&self) -> Option<AgentUser> {
        if let Some(u) = self.agent_user.lock().await.clone() {
            return Some(u);
        }
        let resolved = resolve_agent_user();
        if let Some(ref u) = resolved {
            *self.agent_user.lock().await = Some(u.clone());
        }
        resolved
    }

    /// `systemctl --user <args…> <unit>`, run as the agent user.
    async fn systemctl(
        &self,
        user: Option<&AgentUser>,
        args: &[&str],
    ) -> Result<std::process::Output, String> {
        let bin = systemd_tool("systemctl").ok_or_else(|| "systemctl not found".to_string())?;
        let mut argv: Vec<String> = vec!["--user".to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        argv.push(self.unit.clone());
        let cmd = as_agent_user(user, &bin, &argv, &bus_env(user));
        run_capture(cmd, "systemctl").await
    }

    /// Is the transient unit up? `is-active` exits 0 only for `active`; treat
    /// `activating`/`reloading` as up too so a restart window isn't read as
    /// "the agent is gone" by the supervisor.
    async fn unit_active(&self) -> bool {
        if self.run_mode != RunMode::SystemdUser {
            return false;
        }
        let user = self.agent_user().await;
        match self.systemctl(user.as_ref(), &["is-active"]).await {
            Ok(out) => {
                let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
                matches!(state.as_str(), "active" | "activating" | "reloading")
            }
            Err(e) => {
                warn!("[AgentController] is-active check failed: {}", e);
                false
            }
        }
    }

    /// Unit-name idempotency check. `want_standby = None` means "adopt whatever
    /// mode is running" (used on WMS boot, where we just re-attach to the agent
    /// that survived); `Some(mode)` additionally requires the running unit to be
    /// in that mode.
    async fn try_adopt_running_agent(&self, want_standby: Option<bool>) -> Adopt {
        if self.run_mode != RunMode::SystemdUser {
            return Adopt::NotRunning;
        }
        if !self.unit_active().await {
            return Adopt::NotRunning;
        }
        let env = self.running_agent_env().await;
        adopt_decision(true, env.as_ref(), want_standby)
    }

    /// Recover the running agent's session: its `EnvironmentFile` first (the
    /// authoritative copy — it is exactly what the unit was started with),
    /// falling back to the token persisted on our own device row (which also
    /// survives a WMS restart, since SurrealDB is on disk).
    async fn running_agent_env(&self) -> Option<BTreeMap<String, String>> {
        let user = self.agent_user().await;
        if let Some(map) = self.read_agent_env_file(user.as_ref()).await {
            if map.contains_key("WS_AUTH_TOKEN") {
                return Some(map);
            }
        }
        let token = self.read_token_from_db().await?;
        let mut map = BTreeMap::new();
        map.insert("WS_AUTH_TOKEN".to_string(), token);
        // XELTH_START_MODE deliberately absent = "mode unknown" → the caller
        // does not enforce a mode match against a guess.
        Some(map)
    }

    async fn read_token_from_db(&self) -> Option<String> {
        let v: Option<Value> = self
            .db
            .query("SELECT xelixir_token FROM type::record('registered_device', $iid)")
            .bind(("iid", self.instance_id.clone()))
            .await
            .ok()
            .and_then(|mut r| r.take(0).ok())
            .flatten();
        v.and_then(|val| {
            val.get("xelixir_token")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
    }

    /// Start the agent as a transient `systemd --user` unit.
    ///
    /// Properties worth knowing:
    /// * `--collect` — a failed/stopped transient unit is garbage-collected, so
    ///   the FIXED unit name is free for the next start.
    /// * `Restart=on-failure` + `RestartSec` — the agent self-heals on crash
    ///   even while the WMS is down; the WMS supervisor is only a backstop.
    /// * `StartLimitIntervalSec=0` — no start-rate limiting, matching the old
    ///   "respawn forever every 5 s" behaviour (a rate limit would silently
    ///   retire the only off-site channel).
    /// * `EnvironmentFile=` — the access token, never an argv.
    /// * `SyslogIdentifier=` — the agent keeps its own journal identity (the
    ///   job `systemd-cat` does on the child path).
    async fn start_transient_unit(
        &self,
        user: Option<&AgentUser>,
        agent_path: &Path,
        agent_env: &[(&'static str, String)],
    ) -> Result<(), String> {
        let bin = systemd_tool("systemd-run").ok_or_else(|| "systemd-run not found".to_string())?;
        let env_path = agent_env_path(user);
        self.write_agent_env_file(user, &env_path, agent_env).await?;

        let mut hardened: Vec<String> = vec![
            "-p".into(),
            "Restart=on-failure".into(),
            "-p".into(),
            format!("RestartSec={}", UNIT_RESTART_SEC),
            "-p".into(),
            "StartLimitIntervalSec=0".into(),
            "-p".into(),
            "SyslogIdentifier=xelixir-agent".into(),
            "-p".into(),
            format!("EnvironmentFile={}", env_path),
        ];
        if let Some(u) = user {
            // Deterministic, writable cwd. Without this the unit would inherit
            // the user manager's default (also $HOME) rather than the WMS's cwd
            // — spelling it out keeps the agent's relative paths predictable.
            hardened.push("-p".into());
            hardened.push(format!("WorkingDirectory={}", u.home));
        }
        // Minimal set for older systemd that may reject one of the extras —
        // never lose the agent over a nice-to-have property.
        let minimal: Vec<String> = vec![
            "-p".into(),
            "Restart=on-failure".into(),
            "-p".into(),
            format!("RestartSec={}", UNIT_RESTART_SEC),
            "-p".into(),
            format!("EnvironmentFile={}", env_path),
        ];

        let mut last_err = String::from("systemd-run: no attempt ran");
        for (attempt, props) in [hardened, minimal].into_iter().enumerate() {
            let mut argv: Vec<String> = vec![
                "--user".into(),
                "--quiet".into(),
                "--collect".into(),
                format!("--unit={}", self.unit),
                "--description=xelixir fleet agent (managed by 9eck WMS, own cgroup)".into(),
            ];
            argv.extend(props);
            argv.push("--".into());
            argv.push(agent_path.display().to_string());

            let cmd = as_agent_user(user, &bin, &argv, &bus_env(user));
            match run_capture(cmd, "systemd-run").await {
                Ok(out) if out.status.success() => return Ok(()),
                Ok(out) => {
                    last_err = format!(
                        "systemd-run exit {:?}: {}",
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Err(e) => last_err = e,
            }
            if attempt == 0 {
                warn!(
                    "[AgentController] {} — retrying with the minimal property set",
                    last_err
                );
                // A refused start can leave a half-created/failed unit behind.
                let _ = self.systemctl(user, &["reset-failed"]).await;
            }
        }
        Err(last_err)
    }

    /// Write the agent's environment to a `0600` file owned by the run user.
    ///
    /// When the agent runs as a DIFFERENT user (the kiosk case) the WMS cannot
    /// write into that user's `/run/user/<uid>` at all, so the blob is handed to
    /// a `sudo -u <user> sh` through a **preserved environment variable** — not
    /// an argument — and written under `umask 077`. The token therefore never
    /// appears in `/proc/*/cmdline`, mirroring the hygiene of the old
    /// child-inherits-env approach.
    async fn write_agent_env_file(
        &self,
        user: Option<&AgentUser>,
        path: &str,
        agent_env: &[(&'static str, String)],
    ) -> Result<(), String> {
        let blob = render_env_file(agent_env)?;
        if let Some(u) = user.filter(|u| !is_self_uid(u.uid)) {
            let script = r#"umask 077; printf '%s' "$XLT_AGENT_ENV_BLOB" > "$XLT_AGENT_ENV_PATH""#;
            let mut c = Command::new("sudo");
            c.arg("-n")
                .arg("-u")
                .arg(&u.name)
                .arg("--preserve-env=XLT_AGENT_ENV_BLOB,XLT_AGENT_ENV_PATH")
                .arg("/bin/sh")
                .arg("-c")
                .arg(script);
            c.env("XLT_AGENT_ENV_BLOB", &blob);
            c.env("XLT_AGENT_ENV_PATH", path);
            let out = run_capture(c, "write agent env file").await?;
            if !out.status.success() {
                return Err(format!(
                    "writing {} as {} failed (exit {:?}): {}",
                    path,
                    u.name,
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            return Ok(());
        }
        write_private_file(path, &blob)
    }

    async fn read_agent_env_file(
        &self,
        user: Option<&AgentUser>,
    ) -> Option<BTreeMap<String, String>> {
        let path = agent_env_path(user);
        let raw = if let Some(u) = user.filter(|u| !is_self_uid(u.uid)) {
            let mut c = Command::new("sudo");
            c.arg("-n")
                .arg("-u")
                .arg(&u.name)
                .arg("--preserve-env=XLT_AGENT_ENV_PATH")
                .arg("/bin/sh")
                .arg("-c")
                .arg(r#"cat "$XLT_AGENT_ENV_PATH" 2>/dev/null"#);
            c.env("XLT_AGENT_ENV_PATH", &path);
            let out = run_capture(c, "read agent env file").await.ok()?;
            if !out.status.success() {
                return None;
            }
            String::from_utf8_lossy(&out.stdout).into_owned()
        } else {
            std::fs::read_to_string(&path).ok()?
        };
        let map = parse_env_file(&raw);
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    }

    /// Token hygiene: drop the env file once the session is over.
    async fn clear_agent_env_file(&self, user: Option<&AgentUser>) {
        let path = agent_env_path(user);
        if let Some(u) = user.filter(|u| !is_self_uid(u.uid)) {
            let mut c = Command::new("sudo");
            c.arg("-n")
                .arg("-u")
                .arg(&u.name)
                .arg("--preserve-env=XLT_AGENT_ENV_PATH")
                .arg("/bin/sh")
                .arg("-c")
                .arg(r#"rm -f "$XLT_AGENT_ENV_PATH""#);
            c.env("XLT_AGENT_ENV_PATH", &path);
            let _ = run_capture(c, "remove agent env file").await;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    // ─── DB helpers ───────────────────────────────────────────────────────

    async fn ensure_self_device_record(&self) -> Result<(), String> {
        // Timestamps via time::now(), NOT a chrono string bind — registered_device
        // is a SYNCED table, and a string `updated_at` is the a0c275d/133279d bug
        // class (this writer used to re-poison the row right after the startup heal).
        let q = "
            UPSERT type::record('registered_device', $iid) MERGE {
                device_id: $iid,
                device_name: $name,
                public_key: $pk,
                status: 'active',
                home_instance_id: $iid,
                last_seen_at: time::now(),
                updated_at: time::now(),
                created_at: time::now()
            };
        ";
        self.db
            .query(q)
            .bind(("iid", self.instance_id.clone()))
            .bind((
                "name",
                std::env::var("INSTANCE_NAME").unwrap_or_else(|_| {
                    format!("node-{}", &self.instance_id.chars().take(8).collect::<String>())
                }),
            ))
            .bind(("pk", self.public_key.clone()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn ensure_config(&self) -> XelixirConfig {
        if let Some(cfg) = self.read_config().await {
            return cfg;
        }
        let _ = self
            .db
            .query("UPSERT system_config:xelixir MERGE { auto_start: false, auto_accept: true, updated_at: time::now() };")
            .await;
        XelixirConfig::default()
    }

    async fn read_config(&self) -> Option<XelixirConfig> {
        let v: Option<Value> = self
            .db
            .query("SELECT auto_start, auto_accept FROM system_config:xelixir")
            .await
            .ok()
            .and_then(|mut r| r.take(0).ok())
            .flatten();
        v.map(|val| XelixirConfig {
            auto_start: val
                .get("auto_start")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            auto_accept: val
                .get("auto_accept")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        })
    }

    async fn set_device_state(
        &self,
        status: &str,
        token: Option<String>,
        session_url: Option<String>,
    ) {
        // NOTE: `$token` is a reserved session variable in SurrealDB v3.
        // Bind it as `$xltoken` to avoid the "protected variable" error.
        // Timestamps via time::now() — synced table, string stamps are the
        // a0c275d/133279d bug class.
        let q = "
            UPDATE type::record('registered_device', $iid) MERGE {
                xelixir_status: $status,
                xelixir_token: $xltoken,
                xelixir_session_url: $url,
                xelixir_updated_at: time::now(),
                updated_at: time::now()
            };
        ";
        if let Err(e) = self
            .db
            .query(q)
            .bind(("iid", self.instance_id.clone()))
            .bind(("status", status.to_string()))
            .bind(("xltoken", token.clone()))
            .bind(("url", session_url.clone()))
            .await
        {
            warn!("[AgentController] failed to set device state: {}", e);
        }
        // Enqueue outbox push so the cloud sees the new state quickly.
        self.enqueue_self_outbox().await;
    }

    async fn clear_device_command(&self) {
        let _ = self
            .db
            .query(
                "UPDATE type::record('registered_device', $iid) MERGE { xelixir_command: NONE, updated_at: time::now() };",
            )
            .bind(("iid", self.instance_id.clone()))
            .await;
        self.enqueue_self_outbox().await;
    }

    /// Push our self-row into `sync_outbox` so the SyncEngine's LIVE SELECT
    /// watcher pushes it to peers in real time (no 60s Merkle wait).
    async fn enqueue_self_outbox(&self) {
        let row: Option<Value> = self
            .db
            .query("SELECT *, record::id(id) AS id FROM type::record('registered_device', $iid) LIMIT 1")
            .bind(("iid", self.instance_id.clone()))
            .await
            .ok()
            .and_then(|mut r| r.take(0).ok())
            .flatten();
        let Some(payload) = row else { return };
        let _ = self
            .db
            .query(
                "INSERT INTO sync_outbox { \
                    entity_type: 'registered_device', \
                    entity_id: $eid, \
                    payload: $data, \
                    error_count: 0, \
                    next_attempt_at: time::now(), \
                    created_at: time::now() \
                }",
            )
            .bind(("eid", self.instance_id.clone()))
            .bind(("data", payload))
            .await;
    }

    async fn broadcast_alert(&self, severity: &str, title: &str, message: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let alert = json!({
            "title": title,
            "message": message,
            "severity": severity,
            "status": "unread",
            "mitigated": false,
            "created_at": &now,
            "reported_to_cloud": false
        });
        let _ = self
            .db
            .create::<Option<Value>>("system_alert")
            .content(alert)
            .await;
        let ws_msg = json!({
            "type": "SYSTEM_ALERT",
            "title": title,
            "message": message,
            "severity": severity,
            "timestamp": &now
        });
        if let Ok(s) = serde_json::to_string(&ws_msg) {
            let _ = self.ws_tx.send(s);
        }
    }
}

fn session_url_for_token(token: &str) -> String {
    let base = std::env::var("XELTH_SESSION_BASE")
        .unwrap_or_else(|_| "".to_string());
    format!("{}?token={}", base, token)
}

// ─── run-mode / transient-unit helpers ────────────────────────────────────

/// Absolute path of a systemd tool, if this host has one. Absolute on purpose:
/// these end up in the kiosk's sudoers rules, which match on the full path.
fn systemd_tool(name: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.exists())
}

/// Can we run the agent as a transient user unit here? Needs systemd as PID 1
/// (`/run/systemd/system`) plus both tools. Says nothing about whether the run
/// user actually has a user manager — that failure is caught at spawn time and
/// degrades to the direct-child fallback.
fn systemd_available() -> bool {
    cfg!(target_os = "linux")
        && Path::new("/run/systemd/system").exists()
        && systemd_tool("systemd-run").is_some()
        && systemd_tool("systemctl").is_some()
}

/// `XELIXIR_AGENT_RUN_MODE`: `child`/`direct` forces the historical child
/// spawn, `systemd`/`unit` forces the transient unit, anything else (incl.
/// unset and `auto`) picks systemd when the host supports it.
fn run_mode_from_flag(flag: Option<&str>, systemd_available: bool) -> RunMode {
    match flag.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("child") | Some("direct") | Some("process") => RunMode::DirectChild,
        Some("systemd") | Some("systemd-run") | Some("unit") => RunMode::SystemdUser,
        _ => {
            if systemd_available {
                RunMode::SystemdUser
            } else {
                RunMode::DirectChild
            }
        }
    }
}

fn detect_run_mode() -> RunMode {
    let flag = std::env::var("XELIXIR_AGENT_RUN_MODE").ok();
    run_mode_from_flag(flag.as_deref(), systemd_available())
}

/// FIXED unit name (`XELIXIR_AGENT_UNIT` overrides). The fixed name is the
/// idempotency key: `systemd-run --unit=<name>` refuses to create a second unit
/// with the same name, so even a racing WMS cannot end up with two agents.
fn normalize_unit_name(raw: Option<&str>) -> String {
    let name = raw.unwrap_or("").trim();
    if name.is_empty() {
        return AGENT_UNIT_DEFAULT.to_string();
    }
    if name.ends_with(".service") {
        name.to_string()
    } else {
        format!("{}.service", name)
    }
}

/// uid of this process (Unix only).
#[cfg(unix)]
fn self_uid() -> Option<u32> {
    Some(unsafe { libc::getuid() })
}
#[cfg(not(unix))]
fn self_uid() -> Option<u32> {
    None
}

/// Is `uid` this process's own uid? Then no `sudo` hop is needed.
#[cfg(unix)]
fn is_self_uid(uid: u32) -> bool {
    self_uid() == Some(uid)
}
#[cfg(not(unix))]
fn is_self_uid(_uid: u32) -> bool {
    false
}

/// Environment every `systemd-run`/`systemctl --user` call needs to find the
/// target user's session bus (`sudo` hands us a clean env).
fn bus_env(user: Option<&AgentUser>) -> Vec<(&'static str, String)> {
    match user.map(|u| u.uid).or_else(self_uid) {
        Some(uid) => vec![
            ("XDG_RUNTIME_DIR", format!("/run/user/{}", uid)),
            (
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path=/run/user/{}/bus", uid),
            ),
        ],
        None => Vec::new(),
    }
}

/// Where the unit's `EnvironmentFile` lives. `/run/user/<uid>` is a per-user
/// 0700 tmpfs — the token dies with the session and never touches disk.
fn agent_env_path(user: Option<&AgentUser>) -> String {
    if let Some(uid) = user.map(|u| u.uid).or_else(self_uid) {
        let dir = format!("/run/user/{}", uid);
        if Path::new(&dir).exists() {
            return format!("{}/xelixir-agent.env", dir);
        }
    }
    std::env::temp_dir()
        .join("xelixir-agent.env")
        .to_string_lossy()
        .into_owned()
}

/// Build a `Command` running `program` as the agent user (via `sudo -n -u`),
/// or directly when no separate user applies. `envs` is both the forwarded
/// environment and the sudo allow-list — sudo drops everything else.
fn as_agent_user(
    user: Option<&AgentUser>,
    program: &Path,
    args: &[String],
    envs: &[(&'static str, String)],
) -> Command {
    let mut cmd = match user.filter(|u| !is_self_uid(u.uid)) {
        Some(u) => {
            let mut c = Command::new("sudo");
            c.arg("-n").arg("-u").arg(&u.name);
            if !envs.is_empty() {
                let keys: Vec<&str> = envs.iter().map(|(k, _)| *k).collect();
                c.arg(format!("--preserve-env={}", keys.join(",")));
            }
            c.arg(program);
            c.args(args);
            c
        }
        None => {
            let mut c = Command::new(program);
            c.args(args);
            c
        }
    };
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

/// Run a helper command to completion with a hard timeout. The supervisor loop
/// calls into here on every tick, so a hung `sudo`/dbus must never wedge it —
/// `kill_on_drop` reaps whatever the timeout abandons.
async fn run_capture(mut cmd: Command, what: &str) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(SYSTEMD_CMD_TIMEOUT_SECS), cmd.output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("{}: {}", what, e)),
        Err(_) => Err(format!(
            "{}: timed out after {}s",
            what, SYSTEMD_CMD_TIMEOUT_SECS
        )),
    }
}

/// Serialize the agent env as a systemd `EnvironmentFile`. Values containing a
/// newline are refused rather than silently truncated/injected.
fn render_env_file(kv: &[(&'static str, String)]) -> Result<String, String> {
    let mut out = String::new();
    for (k, v) in kv {
        if v.contains('\n') || v.contains('\r') || v.contains('\0') {
            return Err(format!("refusing to write env {}: embedded newline/NUL", k));
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push('\n');
    }
    Ok(out)
}

/// Parse back what `render_env_file` wrote (plus tolerate `export`-less
/// quoted values, which systemd also accepts).
fn parse_env_file(raw: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(v);
        map.insert(k.trim().to_string(), v.to_string());
    }
    map
}

/// Create/replace a file readable only by its owner (0600 on Unix).
#[cfg(unix)]
fn write_private_file(path: &str, content: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("open {}: {}", path, e))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("write {}: {}", path, e))
}

/// Windows dev fallback — only ever reached in the (unused there) unit path.
#[cfg(not(unix))]
fn write_private_file(path: &str, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("write {}: {}", path, e))
}

/// Pure decision behind [`AgentController::try_adopt_running_agent`] — kept
/// free of I/O so the idempotency rules are unit-testable without systemd.
///
/// `want_standby = None` accepts whatever mode is running; `Some(mode)` demands
/// a match. A running unit whose start mode is UNKNOWN (token recovered from
/// the DB rather than the env file) is adopted as standby — the only mode the
/// UI flags ever spawn — instead of being needlessly restarted.
fn adopt_decision(
    active: bool,
    env: Option<&BTreeMap<String, String>>,
    want_standby: Option<bool>,
) -> Adopt {
    if !active {
        return Adopt::NotRunning;
    }
    let Some(env) = env else {
        return Adopt::Replace;
    };
    let token = match env.get("WS_AUTH_TOKEN").map(|t| t.trim()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return Adopt::Replace,
    };
    match env.get("XELTH_START_MODE") {
        Some(mode) => {
            let standby = mode == "standby";
            if want_standby.is_some_and(|want| want != standby) {
                return Adopt::Replace;
            }
            Adopt::Reattach { token, standby }
        }
        None => Adopt::Reattach {
            token,
            standby: true,
        },
    }
}

/// Resolve the user the xelixir agent should run as. `XELIXIR_AGENT_USER`
/// (a username) overrides; otherwise auto-detect the graphical-session user
/// from the running Wayland compositor's owner.
/// `None` on non-Linux or when no session user is found → caller runs the agent
/// as the WMS uid (historical/headless behaviour).
fn resolve_agent_user() -> Option<AgentUser> {
    if let Ok(name) = std::env::var("XELIXIR_AGENT_USER") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return passwd_lookup(&name).map(|(uid, home)| AgentUser { name, uid, home });
        }
    }
    detect_session_user()
}

/// (uid, home) for a username from /etc/passwd. Linux/Unix only.
fn passwd_lookup(username: &str) -> Option<(u32, String)> {
    for line in std::fs::read_to_string("/etc/passwd").ok()?.lines() {
        let mut f = line.splitn(7, ':');
        let name = f.next()?;
        let _pw = f.next()?;
        let uid: u32 = f.next()?.parse().ok()?;
        let _gid = f.next()?;
        let _gecos = f.next()?;
        let home = f.next()?.to_string();
        if name == username {
            return Some((uid, home));
        }
    }
    None
}

/// (username, home) for a uid from /etc/passwd. Linux/Unix only.
fn passwd_by_uid(uid: u32) -> Option<(String, String)> {
    for line in std::fs::read_to_string("/etc/passwd").ok()?.lines() {
        let mut f = line.splitn(7, ':');
        let name = f.next()?.to_string();
        let _pw = f.next()?;
        let u: u32 = f.next()?.parse().ok()?;
        let _gid = f.next()?;
        let _gecos = f.next()?;
        let home = f.next()?.to_string();
        if u == uid {
            return Some((name, home));
        }
    }
    None
}

/// Find the graphical-session user by scanning /proc for a known Wayland
/// compositor and returning its identity. Mirrors the agent's own
/// `linux_capture::find_session_user`.
fn detect_session_user() -> Option<AgentUser> {
    const COMPOSITORS: &[&str] = &[
        "cage", "sway", "weston", "labwc", "kwin_wayland",
        "gnome-shell", "mutter", "river", "hyprland",
    ];
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let cmdline = match std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let prog = cmdline.split('\0').next().unwrap_or("");
        let base = std::path::Path::new(prog)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if !COMPOSITORS.contains(&base) {
            continue;
        }
        let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(uid) = rest.split_whitespace().next().and_then(|s| s.parse::<u32>().ok()) {
                    if let Some((name, home)) = passwd_by_uid(uid) {
                        return Some(AgentUser { name, uid, home });
                    }
                }
                break;
            }
        }
    }
    None
}

/// Locate the `agent_mock` binary. Resolution order matches the legacy
/// `start_agent_manager` behaviour: cwd → `target/release/` → system path.
fn resolve_agent_binary() -> Option<std::path::PathBuf> {
    let agent_exe = if cfg!(target_os = "windows") {
        "agent_mock.exe"
    } else {
        "agent_mock"
    };
    let cwd_path = std::path::PathBuf::from(agent_exe);
    let target_path = std::env::current_dir()
        .unwrap_or_default()
        .join("target")
        .join("release")
        .join(agent_exe);
    let system_path = if cfg!(target_os = "windows") {
        std::path::PathBuf::from("C:\\Program Files\\xelixir\\agent_mock.exe")
    } else {
        std::path::PathBuf::from("/usr/local/bin/agent_mock")
    };

    if cwd_path.exists() {
        Some(cwd_path)
    } else if target_path.exists() {
        Some(target_path)
    } else if system_path.exists() {
        info!(
            "[AgentController] Using system-installed xelixir agent at {}",
            system_path.display()
        );
        Some(system_path)
    } else {
        error!(
            "[AgentController] xelixir agent binary `{}` not found (cwd, {}, {})",
            agent_exe,
            target_path.display(),
            system_path.display()
        );
        None
    }
}

/// Route the fleet agent's stdout+stderr through `systemd-cat -t xelixir-agent`
/// so its log lines land in the journal under their OWN syslog identifier
/// instead of the 9eck-wms unit's (the 95%-noise problem — see xelixir
/// `.eck/TECH_DEBT.md`). We spawn systemd-cat with a piped stdin and hand a
/// CLOEXEC dup of that pipe's write-end to the agent as both stdout and stderr;
/// the agent itself stays a plain child handle, so the caller's pid tracking,
/// respawn-on-crash, and kill paths are untouched.
///
/// On success returns the systemd-cat `Child` (the caller reaps it once the
/// agent exits — it EOFs and dies on its own then). On ANY failure (systemd-cat
/// not on PATH, dup failure) it wires the agent to inherit WMS's fds — the
/// previous behaviour — and returns `None`, so the agent still spawns.
#[cfg(unix)]
fn spawn_agent_log_forwarder(agent_cmd: &mut Command) -> Option<Child> {
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

    fn inherit(c: &mut Command) {
        c.stdout(Stdio::inherit());
        c.stderr(Stdio::inherit());
    }

    let mut cat = match Command::new("systemd-cat")
        .arg("-t")
        .arg("xelixir-agent")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "[AgentController] systemd-cat unavailable ({}); agent logs inherit the WMS journal",
                e
            );
            inherit(agent_cmd);
            return None;
        }
    };

    let stdin = match cat.stdin.take() {
        Some(s) => s,
        None => {
            warn!("[AgentController] systemd-cat stdin missing; agent logs inherit the WMS journal");
            inherit(agent_cmd); // `cat` drops here → kill_on_drop reaps it
            return None;
        }
    };

    // Duplicate the pipe write-end (CLOEXEC) so BOTH agent streams can target
    // it, then drop the WMS-held copy so systemd-cat sees EOF exactly when the
    // agent exits — not while WMS still holds a write end open.
    let raw = stdin.as_raw_fd();
    let out_fd = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
    let err_fd = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
    if out_fd < 0 || err_fd < 0 {
        if out_fd >= 0 {
            unsafe { libc::close(out_fd); }
        }
        if err_fd >= 0 {
            unsafe { libc::close(err_fd); }
        }
        warn!("[AgentController] dup of systemd-cat pipe failed; agent logs inherit the WMS journal");
        inherit(agent_cmd); // `cat` drops → kill_on_drop reaps it
        return None;
    }
    // SAFETY: fresh fds from F_DUPFD_CLOEXEC that nothing else owns.
    let out_owned = unsafe { OwnedFd::from_raw_fd(out_fd) };
    let err_owned = unsafe { OwnedFd::from_raw_fd(err_fd) };
    drop(stdin);

    agent_cmd.stdout(Stdio::from(out_owned));
    agent_cmd.stderr(Stdio::from(err_owned));
    Some(cat)
}

/// Non-unix fallback: `systemd-cat` is Linux-only. Keep the historical
/// inherited-journal behaviour so Windows dev builds still compile.
#[cfg(not(unix))]
fn spawn_agent_log_forwarder(agent_cmd: &mut Command) -> Option<Child> {
    agent_cmd.stdout(Stdio::inherit());
    agent_cmd.stderr(Stdio::inherit());
    None
}

async fn mark_stopped_in_db(db: &SurrealDb, iid: &str) -> Result<(), String> {
    db.query(
        "UPDATE type::record('registered_device', $iid) MERGE { \
            xelixir_status: 'stopped', \
            xelixir_token: NONE, \
            xelixir_session_url: NONE, \
            xelixir_updated_at: time::now(), \
            updated_at: time::now() \
        };",
    )
    .bind(("iid", iid.to_string()))
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── run-mode selection ────────────────────────────────────────────────

    #[test]
    fn auto_picks_systemd_only_when_available() {
        assert_eq!(run_mode_from_flag(None, true), RunMode::SystemdUser);
        assert_eq!(run_mode_from_flag(None, false), RunMode::DirectChild);
        assert_eq!(run_mode_from_flag(Some("auto"), true), RunMode::SystemdUser);
        assert_eq!(run_mode_from_flag(Some("auto"), false), RunMode::DirectChild);
    }

    #[test]
    fn flag_overrides_detection_both_ways() {
        // Forced child even on a systemd host (dev boxes / tests).
        assert_eq!(run_mode_from_flag(Some("child"), true), RunMode::DirectChild);
        assert_eq!(run_mode_from_flag(Some(" Direct "), true), RunMode::DirectChild);
        // Forced unit even when detection says no (operator knows better;
        // a genuine failure still degrades to a child at spawn time).
        assert_eq!(run_mode_from_flag(Some("systemd"), false), RunMode::SystemdUser);
        assert_eq!(run_mode_from_flag(Some("unit"), false), RunMode::SystemdUser);
    }

    // ── unit-name idempotency ─────────────────────────────────────────────

    #[test]
    fn unit_name_is_fixed_and_normalized() {
        assert_eq!(normalize_unit_name(None), AGENT_UNIT_DEFAULT);
        assert_eq!(normalize_unit_name(Some("   ")), AGENT_UNIT_DEFAULT);
        assert_eq!(normalize_unit_name(Some("kiosk-agent")), "kiosk-agent.service");
        assert_eq!(
            normalize_unit_name(Some(" kiosk-agent.service ")),
            "kiosk-agent.service"
        );
    }

    #[test]
    fn no_unit_running_means_spawn() {
        assert_eq!(adopt_decision(false, None, Some(true)), Adopt::NotRunning);
        // Even a leftover env file must not fake a running agent.
        let env = env_of(&[("WS_AUTH_TOKEN", "tok")]);
        assert_eq!(adopt_decision(false, Some(&env), Some(true)), Adopt::NotRunning);
    }

    #[test]
    fn running_unit_in_same_mode_is_reattached_not_duplicated() {
        let env = env_of(&[("WS_AUTH_TOKEN", "tok-1"), ("XELTH_START_MODE", "standby")]);
        assert_eq!(
            adopt_decision(true, Some(&env), Some(true)),
            Adopt::Reattach {
                token: "tok-1".into(),
                standby: true
            }
        );
        // WMS boot (mode-agnostic re-attach) adopts whatever is running.
        assert_eq!(
            adopt_decision(true, Some(&env), None),
            Adopt::Reattach {
                token: "tok-1".into(),
                standby: true
            }
        );
    }

    #[test]
    fn mode_mismatch_or_lost_token_replaces_never_duplicates() {
        let standby = env_of(&[("WS_AUTH_TOKEN", "tok"), ("XELTH_START_MODE", "standby")]);
        // A live (non-standby) start requested while a standby unit runs.
        assert_eq!(adopt_decision(true, Some(&standby), Some(false)), Adopt::Replace);
        // Session unrecoverable: env file gone, and no token on the device row.
        assert_eq!(adopt_decision(true, None, Some(true)), Adopt::Replace);
        // Env file present but tokenless / blank.
        let blank = env_of(&[("WS_AUTH_TOKEN", "   "), ("E9_INSTANCE_ID", "iid")]);
        assert_eq!(adopt_decision(true, Some(&blank), Some(true)), Adopt::Replace);
        let no_tok = env_of(&[("E9_INSTANCE_ID", "iid")]);
        assert_eq!(adopt_decision(true, Some(&no_tok), Some(true)), Adopt::Replace);
    }

    #[test]
    fn unknown_start_mode_adopts_as_standby() {
        // Token recovered from the DB row → mode unknown. Adopt rather than
        // restart the one channel we have.
        let env = env_of(&[("WS_AUTH_TOKEN", "tok")]);
        assert_eq!(
            adopt_decision(true, Some(&env), Some(true)),
            Adopt::Reattach {
                token: "tok".into(),
                standby: true
            }
        );
    }

    // ── env-file handover ─────────────────────────────────────────────────

    #[test]
    fn env_file_round_trips() {
        let kv: Vec<(&'static str, String)> = vec![
            ("WS_AUTH_TOKEN", "abc.DEF-123_/+=".to_string()),
            ("E9_INSTANCE_ID", "0e1e5ca0-dead-beef".to_string()),
            ("XELTH_WS_URL", "".to_string()),
            ("XELTH_START_MODE", "standby".to_string()),
        ];
        let rendered = render_env_file(&kv).expect("renders");
        let parsed = parse_env_file(&rendered);
        for (k, v) in &kv {
            assert_eq!(parsed.get(*k), Some(v), "key {}", k);
        }
        assert_eq!(
            adopt_decision(true, Some(&parsed), Some(true)),
            Adopt::Reattach {
                token: "abc.DEF-123_/+=".into(),
                standby: true
            }
        );
    }

    #[test]
    fn env_file_refuses_injection() {
        let kv: Vec<(&'static str, String)> =
            vec![("WS_AUTH_TOKEN", "tok\nExecStart=/bin/evil".to_string())];
        assert!(render_env_file(&kv).is_err());
    }

    #[test]
    fn env_file_parser_skips_noise() {
        let parsed = parse_env_file("# comment\n\nWS_AUTH_TOKEN=\"quoted\"\ngarbage-line\n");
        assert_eq!(parsed.get("WS_AUTH_TOKEN").map(String::as_str), Some("quoted"));
        assert_eq!(parsed.len(), 1);
    }
}
