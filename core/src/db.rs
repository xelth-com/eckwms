use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;
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
/// connect to the shared server, `signin` as Root (from `SURREAL_ROOT_USER` /
/// `SURREAL_ROOT_PASS` when present), and select namespace `SURREAL_NS`
/// (default `eck`) + database `db_name`. Multiple engines share one server by
/// each using a distinct namespace (`SURREAL_NS=eck1|eck2|eck3`); the Zone-1 /
/// Zone-2 split stays as the `db_name` (`main` / `users`) within that namespace.
///
/// **Embedded mode** (default): SurrealKv at `path`, namespace `eck`, database
/// `db_name` — the original behavior for edge nodes / kiosks.
pub async fn connect_with_db(path: &str, db_name: &str) -> Result<SurrealDb, surrealdb::Error> {
    if let Ok(remote) = std::env::var("SURREAL_REMOTE_URL") {
        let remote = remote.trim();
        if !remote.is_empty() {
            let db = any::connect(remote).await?;
            if let (Ok(user), Ok(pass)) = (
                std::env::var("SURREAL_ROOT_USER"),
                std::env::var("SURREAL_ROOT_PASS"),
            ) {
                if !user.is_empty() {
                    db.signin(Root {
                        username: user,
                        password: pass,
                    })
                    .await?;
                }
            }
            let ns = std::env::var("SURREAL_NS").unwrap_or_else(|_| "eck".to_string());
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
