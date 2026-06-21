// Regression guard for SurrealDB issue #7199 — @N@ + $bind silently returning 0 rows.
// Fixed in 3.0.5 (PR #7264). This test fails if a future SurrealDB version reintroduces it.
// Run: cargo test -p wms --test bm25_bind_check -- --nocapture

use serde_json::Value;
use surrealdb::engine::local::Mem;
use surrealdb::Surreal;

#[tokio::test]
async fn bm25_match_with_bind_variable() {
    let db = Surreal::new::<Mem>(()).await.unwrap();
    db.use_ns("t").use_db("t").await.unwrap();

    db.query(
        "DEFINE ANALYZER ascii TOKENIZERS blank,class,camel,punct FILTERS lowercase,ascii;
         DEFINE INDEX title_ft ON book FIELDS title FULLTEXT ANALYZER ascii BM25;
         INSERT INTO book { title: 'Hello world' };
         INSERT INTO book { title: 'Goodbye moon' };",
    )
    .await
    .unwrap()
    .check()
    .unwrap();

    let inlined: Vec<Value> = db
        .query("SELECT *, record::id(id) AS id FROM book WHERE title @1@ 'Hello'")
        .await
        .unwrap()
        .take(0)
        .unwrap();

    let bound: Vec<Value> = db
        .query("SELECT *, record::id(id) AS id FROM book WHERE title @1@ $q")
        .bind(("q", "Hello".to_string()))
        .await
        .unwrap()
        .take(0)
        .unwrap();

    assert_eq!(inlined.len(), 1, "inline literal must match 1 row");
    assert_eq!(
        bound.len(),
        1,
        "bind variant returned {} — issue #7199 regressed in this SurrealDB build",
        bound.len()
    );
}
