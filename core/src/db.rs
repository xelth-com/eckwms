use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::{Namespace, Root};
use surrealdb::Surreal;
use tracing::info;

/// Backend-agnostic SurrealDB handle. Resolves at runtime to either the
/// embedded SurrealKv engine (edge nodes / kiosks) or a remote SurrealDB server
/// over WebSocket (shared `:8000` for the paid eck1/eck2/eck3 engines) — see B1
/// in `.eck/ENTERPRISE_CLUSTER.md`. Using the `any` engine keeps every handler
/// (`&SurrealDb`) identical regardless of where the data physically lives.
pub type SurrealDb = Surreal<Any>;

/// Initialize a SurrealDB connection (db `main`). See [`connect_with_db`].
pub async fn connect(path: &str) -> Result<SurrealDb, surrealdb::Error> {
    connect_with_db(path, "main").await
}

/// Connect to SurrealDB, picking the database within the namespace.
///
/// **Remote mode** (when `SURREAL_REMOTE_URL` is set, e.g. `ws://127.0.0.1:8000`):
/// connect to the shared server and select namespace `SURREAL_NS` (default
/// `eck`) + database `db_name`. Multiple engines share one server by each
/// using a distinct namespace (`SURREAL_NS=eck1|eck2|eck3`); the Zone-1 /
/// Zone-2 split stays as the `db_name` (`main` / `users`) within that namespace.
///
/// Auth (in order of preference):
/// 1. `SURREAL_NS_USER` / `SURREAL_NS_PASS` — a namespace-scoped user
///    (`DEFINE USER ... ON NAMESPACE`), which cannot touch other namespaces
///    on the shared server. Preferred for isolation.
/// 2. `SURREAL_ROOT_USER` / `SURREAL_ROOT_PASS` (falling back to
///    `SURREAL_USER` / `SURREAL_PASS`, the names used by the host-shared
///    `/etc/surrealdb/root.env`) — root signin.
///
/// **Embedded mode** (default): SurrealKv at `path`, namespace `eck`, database
/// `db_name` — the original behavior for edge nodes / kiosks.
pub async fn connect_with_db(path: &str, db_name: &str) -> Result<SurrealDb, surrealdb::Error> {
    if let Ok(remote) = std::env::var("SURREAL_REMOTE_URL") {
        let remote = remote.trim();
        if !remote.is_empty() {
            let db = any::connect(remote).await?;
            let ns = std::env::var("SURREAL_NS").unwrap_or_else(|_| "eck".to_string());
            let ns_user = std::env::var("SURREAL_NS_USER").unwrap_or_default();
            if !ns_user.is_empty() {
                db.signin(Namespace {
                    namespace: ns.clone(),
                    username: ns_user,
                    password: std::env::var("SURREAL_NS_PASS").unwrap_or_default(),
                })
                .await?;
            } else {
                let user = std::env::var("SURREAL_ROOT_USER")
                    .or_else(|_| std::env::var("SURREAL_USER"))
                    .unwrap_or_default();
                if !user.is_empty() {
                    let pass = std::env::var("SURREAL_ROOT_PASS")
                        .or_else(|_| std::env::var("SURREAL_PASS"))
                        .unwrap_or_default();
                    db.signin(Root {
                        username: user,
                        password: pass,
                    })
                    .await?;
                }
            }
            db.use_ns(ns.clone()).use_db(db_name).await?;
            info!(
                "SurrealDB connected (remote {}) ns={} db={}",
                remote, ns, db_name
            );
            return Ok(db);
        }
    }

    // Embedded SurrealKv via the `any` engine (scheme `surrealkv://`).
    let endpoint = format!("surrealkv://{path}");
    let db = any::connect(endpoint).await?;
    db.use_ns("eck").use_db(db_name).await?;
    info!("SurrealDB connected (SurrealKv) at: {} (db={})", path, db_name);
    Ok(db)
}
