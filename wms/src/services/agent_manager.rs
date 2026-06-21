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

use std::sync::Arc;
use std::process::Stdio;

use serde_json::{json, Value};
use surrealdb::types::SurrealValue;
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{debug, error, info, warn};
use futures_util::StreamExt;

use eck_core::db::SurrealDb;

/// Handle to a running `agent_mock` child. Dropping the controller's
/// `Option<AgentHandle>` triggers the kill signal; the spawned wait task
/// terminates the child and clears the DB token.
struct AgentHandle {
    kill: oneshot::Sender<()>,
}

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
const RESPAWN_BACKOFF_SECS: u64 = 5;

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
    handle: Mutex<Option<AgentHandle>>,
}

impl AgentController {
    pub fn new(
        db: SurrealDb,
        ws_tx: broadcast::Sender<String>,
        instance_id: String,
        public_key: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            ws_tx,
            instance_id,
            public_key,
            handle: Mutex::new(None),
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

        // "Auto-start at boot" spawns the agent in STANDBY (DNO poll/dormant) —
        // never a live connection. Live happens only on an on-demand wake.
        if cfg.auto_start {
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
        // respawn it if it died (crash / OOM / OTA self-exec). The agent's wait
        // task clears the handle on exit; we respawn HERE (not in the wait task —
        // that would make its future recursively contain spawn_agent). Mirrors
        // the old standalone systemd unit's Restart=always. Always respawned in
        // standby (the only flag-driven spawn mode).
        {
            let sup = Arc::clone(&self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(RESPAWN_BACKOFF_SECS)).await;
                    let auto = sup.read_config().await.map(|c| c.auto_start).unwrap_or(false);
                    if auto && sup.handle.lock().await.is_none() {
                        warn!("[AgentController] supervisor: agent down + auto_start=on — respawning (standby)");
                        match sup.spawn_agent(true).await {
                            Ok(token) => {
                                let url = session_url_for_token(&token);
                                sup.set_device_state("standby", Some(token), Some(url)).await;
                            }
                            Err(e) => warn!("[AgentController] supervisor respawn failed: {}", e),
                        }
                    }
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

    /// Claim a license token at xelth.com, then spawn `agent_mock` with the
    /// returned WS access token. Replaces any currently-running child.
    ///
    /// `standby=true` forwards `XELTH_START_MODE=standby` so the agent polls
    /// xelixir until woken instead of holding a live socket — the only mode the
    /// two UI flags spawn. `standby=false` (live/always-connected) is reserved
    /// for the future WMS self-heal mode (paid), not wired to the flags.
    pub async fn spawn_agent(self: &Arc<Self>, standby: bool) -> Result<String, String> {
        // Replace any existing handle first.
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
        // reached on :3221, not via nginx :443. nginx `location /api/` on
        // xelth.com does `return 301 …:3221`, and a 301 makes HTTP clients
        // downgrade POST→GET → the claim (a POST) comes back 405. So the :443
        // form silently breaks license claims; mirror XELTH_WS_URL (:3221).
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
        let agent_user = resolve_agent_user();

        // Prefer the session user's own ~/bin/agent_mock (user-writable → OTA
        // self-update can swap it w/o root); else the resolved system path.
        let agent_path = agent_user
            .as_ref()
            .map(|(_, _, home)| std::path::PathBuf::from(home).join("bin").join("agent_mock"))
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

        let mut agent_cmd = if let Some((ref user, uid, _)) = agent_user {
            // Drop to the session user via sudo, forwarding the Wayland session
            // env so screenshots/input reach the kiosk display. WMS's uid
            // (9eckwms) needs `NOPASSWD: SETENV: (<user>) <agent_path>`.
            let runtime_dir = format!("/run/user/{}", uid);
            info!(
                "[AgentController] spawning agent as session user '{}' (uid {}), bin {}, display {}",
                user, uid, agent_path.display(), wayland_display
            );
            let mut c = Command::new("sudo");
            // --preserve-env strips XELTH_START_MODE otherwise, so add it to the
            // allow-list only in standby (keeps the live path's env unchanged).
            let preserve = if standby {
                "--preserve-env=WS_AUTH_TOKEN,E9_INSTANCE_ID,XELTH_WS_URL,XDG_RUNTIME_DIR,WAYLAND_DISPLAY,XELTH_START_MODE"
            } else {
                "--preserve-env=WS_AUTH_TOKEN,E9_INSTANCE_ID,XELTH_WS_URL,XDG_RUNTIME_DIR,WAYLAND_DISPLAY"
            };
            c.arg("-n")
                .arg("-u").arg(user)
                .arg(preserve)
                .arg(&agent_path);
            c.env("WS_AUTH_TOKEN", &ws_auth_token);
            c.env("E9_INSTANCE_ID", &self.instance_id);
            c.env("XELTH_WS_URL", &xelth_ws_url);
            c.env("XDG_RUNTIME_DIR", &runtime_dir);
            c.env("WAYLAND_DISPLAY", &wayland_display);
            if standby {
                c.env("XELTH_START_MODE", "standby");
            }
            c
        } else {
            // No session user resolvable (headless host, or Windows): run
            // in-process as the WMS uid — historical behaviour.
            let mut c = Command::new(&agent_path);
            c.env("WS_AUTH_TOKEN", &ws_auth_token);
            c.env("E9_INSTANCE_ID", &self.instance_id);
            c.env("XELTH_WS_URL", &xelth_ws_url);
            if standby {
                c.env("XELTH_START_MODE", "standby");
            }
            c
        };
        agent_cmd.stdout(Stdio::inherit());
        agent_cmd.stderr(Stdio::inherit());
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
        });

        *self.handle.lock().await = Some(AgentHandle { kill: kill_tx });
        Ok(ws_auth_token)
    }

    /// Send the kill signal to any running agent and clear the handle.
    pub async fn stop_agent(&self) {
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = handle.kill.send(());
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    // ─── DB helpers ───────────────────────────────────────────────────────

    async fn ensure_self_device_record(&self) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        let q = "
            UPSERT type::record('registered_device', $iid) MERGE {
                device_id: $iid,
                device_name: $name,
                public_key: $pk,
                status: 'active',
                home_instance_id: $iid,
                last_seen_at: $now,
                updated_at: $now,
                created_at: $now
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
            .bind(("now", now))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn ensure_config(&self) -> XelixirConfig {
        if let Some(cfg) = self.read_config().await {
            return cfg;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let _ = self
            .db
            .query("UPSERT system_config:xelixir MERGE { auto_start: false, auto_accept: true, updated_at: $now };")
            .bind(("now", now))
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
        let now = chrono::Utc::now().to_rfc3339();
        // NOTE: `$token` is a reserved session variable in SurrealDB v3.
        // Bind it as `$xltoken` to avoid the "protected variable" error.
        let q = "
            UPDATE type::record('registered_device', $iid) MERGE {
                xelixir_status: $status,
                xelixir_token: $xltoken,
                xelixir_session_url: $url,
                xelixir_updated_at: $now,
                updated_at: $now
            };
        ";
        if let Err(e) = self
            .db
            .query(q)
            .bind(("iid", self.instance_id.clone()))
            .bind(("status", status.to_string()))
            .bind(("xltoken", token.clone()))
            .bind(("url", session_url.clone()))
            .bind(("now", now.clone()))
            .await
        {
            warn!("[AgentController] failed to set device state: {}", e);
        }
        // Enqueue outbox push so the cloud sees the new state quickly.
        self.enqueue_self_outbox().await;
    }

    async fn clear_device_command(&self) {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = self
            .db
            .query(
                "UPDATE type::record('registered_device', $iid) MERGE { xelixir_command: NONE, updated_at: $now };",
            )
            .bind(("iid", self.instance_id.clone()))
            .bind(("now", now))
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

/// Resolve the user the xelixir agent should run as. `XELIXIR_AGENT_USER`
/// (a username) overrides; otherwise auto-detect the graphical-session user
/// from the running Wayland compositor's owner. Returns `(username, uid, home)`.
/// `None` on non-Linux or when no session user is found → caller runs the agent
/// in-process as the WMS uid (historical/headless behaviour).
fn resolve_agent_user() -> Option<(String, u32, String)> {
    if let Ok(name) = std::env::var("XELIXIR_AGENT_USER") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return passwd_lookup(&name).map(|(uid, home)| (name, uid, home));
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
/// compositor and returning its `(username, uid, home)`. Mirrors the agent's
/// own `linux_capture::find_session_user`.
fn detect_session_user() -> Option<(String, u32, String)> {
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
                        return Some((name, uid, home));
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

async fn mark_stopped_in_db(db: &SurrealDb, iid: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    db.query(
        "UPDATE type::record('registered_device', $iid) MERGE { \
            xelixir_status: 'stopped', \
            xelixir_token: NONE, \
            xelixir_session_url: NONE, \
            xelixir_updated_at: $now, \
            updated_at: $now \
        };",
    )
    .bind(("iid", iid.to_string()))
    .bind(("now", now))
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}
