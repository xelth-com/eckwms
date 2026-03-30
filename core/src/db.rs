use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;
use tracing::info;

pub type SurrealDb = Surreal<Db>;

/// Initialize a SurrealDB connection using the local SurrealKv engine.
/// `path` is the directory where SurrealKv stores its data files.
pub async fn connect(path: &str) -> Result<SurrealDb, surrealdb::Error> {
    let db = Surreal::new::<SurrealKv>(path).await?;
    db.use_ns("eck").use_db("main").await?;
    info!("SurrealDB connected (SurrealKv) at: {}", path);
    Ok(db)
}
