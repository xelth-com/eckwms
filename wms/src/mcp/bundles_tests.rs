//! Tests for the examination-bundle write path (`mcp::bundles`), on a `mem://`
//! SurrealDB and a per-test temp directory as the CAS/staging root.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn test_db() -> SurrealDb {
    let db = surrealdb::engine::any::connect("mem://").await.unwrap();
    db.use_ns("test").use_db("test").await.unwrap();
    db.query(
        "DEFINE TABLE IF NOT EXISTS document SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS;
         DEFINE TABLE OVERWRITE has_attachment TYPE RELATION SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS device SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS device_event SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS bundle_staging SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS protocol_reservation SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS protocol_counter SCHEMALESS;
         DEFINE TABLE IF NOT EXISTS mcp_write_audit SCHEMALESS;",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    db.query(
        "CREATE type::record('document', '53451000000031885') CONTENT { \
            type: 'support_ticket', meta: { ticket_number: '31885', serial_number: 'I824000067' } }",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    db
}

fn temp_base() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "wms-bundles-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().to_string()
}

fn env<'a>(db: &'a SurrealDb, base: &str, writer: bool) -> BundleEnv<'a> {
    BundleEnv { db, base_dir: base.to_string(), caller: "test:master".into(), writer }
}

async fn count(db: &SurrealDb, q: &str) -> usize {
    let v: Vec<Value> = db.query(q).await.unwrap().take(0).unwrap();
    v.len()
}

struct F {
    role: &'static str,
    name: String,
    bytes: Vec<u8>,
}

impl F {
    fn new(role: &'static str, name: &str, bytes: &[u8]) -> Self {
        F { role, name: name.into(), bytes: bytes.to_vec() }
    }
    fn sha(&self) -> String {
        sha256_hex(&self.bytes)
    }
}

/// Upload `f` split into `parts` chunks, in the given chunk order.
async fn upload(e: &BundleEnv<'_>, bundle: &str, f: &F, parts: usize, order: &[usize]) -> BResult<Value> {
    let size = f.bytes.len();
    let step = size.div_ceil(parts).max(1);
    let chunks: Vec<&[u8]> = if size == 0 { vec![&[][..]] } else { f.bytes.chunks(step).collect() };
    let mut last = Value::Null;
    for &i in order {
        last = run_tool(
            e,
            "bundle_upload_chunk",
            &json!({
                "bundle_id": bundle, "role": f.role, "file_name": f.name,
                "size_bytes": size, "sha256": f.sha(), "chunks_total": chunks.len(),
                "chunk_index": i, "content_base64": B64.encode(chunks[i]),
            }),
        )
        .await?;
    }
    Ok(last)
}

fn manifest_for(files: &[&F], protocol_no: &str, serial: &str) -> Value {
    let mut m = Map::new();
    for f in files {
        m.insert(f.name.clone(), json!(f.sha()));
    }
    json!({ "bundle": "acme-en62353-test", "dut": serial, "files": m,
            "hash_alg": "sha256", "protocol_no": protocol_no })
}

fn event(protocol_no: &str, kind: &str, result: &str, date: &str, next_due: Value) -> Value {
    json!({
        "kind": kind, "date": date, "protocol_no": protocol_no, "revision": 0,
        "result": result, "next_due": next_due, "examiner": "Test Examiner",
        "instrument": { "type": "SECUTEST", "serial": "KG1", "calib_due": "2028-01-31" },
        "standard": "DIN EN 62353:2015-10", "legal_basis": "MPBetreibV", "scope": "test",
        "operator_snapshot": { "name": "Praxis Beispiel", "address": "Teststr. 1" },
        "notes": "n", "supersedes_protocol_no": null,
    })
}

/// A full standard bundle (pdf in 3 chunks out of order + duplicate, html,
/// payload, manifest file) staged and ready to commit. Returns the manifest.
async fn stage_standard(e: &BundleEnv<'_>, bundle: &str, serial: &str, protocol_no: &str, ticket: Option<&str>) -> Value {
    let pdf = F::new("protocol_pdf", "P.pdf", &[7u8; 3000]);
    let html = F::new("protocol_html", "P_rev0.html", format!("<html>{bundle}</html>").as_bytes());
    let payload = F::new("tester_payload", "run1.protocol_data.b64", b"QUJDREVG");
    // Manifest names the html under a DIFFERENT name (amendment A2: by sha).
    let mut renamed = F::new("protocol_html", "P.html", &html.bytes);
    renamed.role = "protocol_html";
    let manifest = manifest_for(&[&renamed, &payload], protocol_no, serial);
    let mfile = F::new("manifest", "P.manifest.json", manifest.to_string().as_bytes());
    let mut begin = json!({ "bundle_id": bundle, "serial": serial, "model": "Acme 770",
                            "manifest": manifest, "schema_version": 1 });
    if let Some(t) = ticket {
        begin["ticket_id"] = json!(t);
    }
    run_tool(e, "bundle_begin", &begin).await.unwrap();
    let r = upload(e, bundle, &pdf, 3, &[2, 0, 0, 1]).await.unwrap();
    assert_eq!(r["complete"], json!(true));
    assert_eq!(r["sha256_ok"], json!(true));
    upload(e, bundle, &html, 1, &[0]).await.unwrap();
    upload(e, bundle, &payload, 1, &[0]).await.unwrap();
    upload(e, bundle, &mfile, 1, &[0]).await.unwrap();
    manifest
}

#[tokio::test]
async fn commit_happy_path_out_of_order_chunks_and_stable_receipt() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);

    let b = run_tool(&e, "bundle_begin", &json!({
        "bundle_id": "b-happy", "serial": " i824000067 ", "ticket_id": "31885",
        "manifest": {}, "schema_version": 1 })).await.unwrap();
    assert_eq!(b["device_created"], json!(true));
    assert_eq!(b["ticket"], json!("document:53451000000031885"));

    let num = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "b-happy" }))
        .await
        .unwrap();
    let pno = num["number"].as_str().unwrap().to_string();
    assert_eq!(pno, "2026-0001");

    // Duplicate chunk: second upload of chunk 0 is ignored, progress unchanged.
    let pdf = F::new("protocol_pdf", "X.pdf", &[1u8; 10]);
    let r = upload(&e, "b-happy", &pdf, 2, &[0, 0]).await.unwrap();
    assert_eq!(r["duplicate"], json!(true));
    assert_eq!(r["received"], json!(1));
    assert_eq!(r["complete"], json!(false));
    // re-begin with the real manifest (idempotent while open; keeps files)
    let manifest = stage_standard(&e, "b-happy", "I824000067", &pno, Some("document:53451000000031885")).await;
    let r = upload(&e, "b-happy", &pdf, 2, &[1]).await.unwrap();
    assert_eq!(r["complete"], json!(true));

    let commit = json!({ "bundle_id": "b-happy",
        "event": event(&pno, "STK", "PASS_WITH_NOTES", "2026-10-28", json!("2027-11")) });
    let receipt = run_tool(&e, "bundle_commit", &commit).await.unwrap();
    assert_eq!(receipt["protocol_no"], json!(pno));
    let files = receipt["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    assert_eq!(receipt["manifest_covered"], json!(2));
    for f in files {
        let rows: Vec<Value> = db
            .query("SELECT VALUE storage_path FROM file_resource WHERE cas_uuid = $c")
            .bind(("c", f["cas_uuid"].as_str().unwrap().to_string()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let path = rows[0].as_str().unwrap();
        assert!(Path::new(&base).join(path).exists(), "CAS file {path} landed");
    }
    assert_eq!(count(&db, "SELECT * FROM device_event").await, 1);
    // 5 files × (event + ticket) edges, with provenance.
    assert_eq!(count(&db, "SELECT * FROM has_attachment WHERE bundle_id = 'b-happy'").await, 10);
    assert_eq!(
        count(&db, "SELECT * FROM has_attachment WHERE in = type::record('document', '53451000000031885')").await,
        5
    );
    assert!(!staging_dir(&base, "b-happy").exists(), "tmp bytes removed");

    // Identical retry → identical receipt, nothing duplicated.
    let again = run_tool(&e, "bundle_commit", &commit).await.unwrap();
    assert_eq!(again, receipt);
    assert_eq!(count(&db, "SELECT * FROM has_attachment").await, 10);
    // A4: re-begin after commit → committed + receipt; upload of a known file → already_committed.
    let rb = run_tool(&e, "bundle_begin", &json!({ "bundle_id": "b-happy", "serial": "I824000067",
        "ticket_id": "31885", "manifest": manifest, "schema_version": 1 })).await.unwrap();
    assert_eq!(rb["state"], json!("committed"));
    assert_eq!(rb["receipt"], receipt);
    let up = upload(&e, "b-happy", &pdf, 1, &[0]).await.unwrap();
    assert_eq!(up["already_committed"], json!(true));
    let res = load_row(&db, "protocol_reservation", &pno).await.unwrap().unwrap();
    assert_eq!(res["state"], json!("committed"));
    let audits = count(&db, "SELECT * FROM mcp_write_audit WHERE caller = 'test:master'").await;
    assert!(audits >= 10, "every write call audited ({audits})");
}

#[tokio::test]
async fn commit_with_wrong_sha_writes_nothing() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    run_tool(&e, "bundle_begin", &json!({ "bundle_id": "b-badsha", "serial": "F91700844", "manifest": {} }))
        .await
        .unwrap();
    // Declared sha does not match the bytes → rejected on completion, file reset.
    let r = run_tool(&e, "bundle_upload_chunk", &json!({
        "bundle_id": "b-badsha", "role": "protocol_pdf", "file_name": "a.pdf", "size_bytes": 3,
        "sha256": "0".repeat(64), "chunks_total": 1, "chunk_index": 0,
        "content_base64": B64.encode(b"abc") })).await;
    assert_eq!(r.unwrap_err().code, "sha256_mismatch");
    let num = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "b-badsha" }))
        .await
        .unwrap();
    let ev = event(num["number"].as_str().unwrap(), "STK", "PASS", "2026-10-01", json!("2027-10"));
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-badsha", "event": ev.clone(),
        "required_roles": [] })).await.unwrap_err();
    assert_eq!(err.code, "incomplete");

    // A staged file tampered on disk after verification → commit refuses.
    let f = F::new("protocol_pdf", "a.pdf", b"abc");
    upload(&e, "b-badsha", &f, 1, &[0]).await.unwrap();
    let tmp = std::fs::read_dir(staging_dir(&base, "b-badsha")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(tmp.join("assembled.bin"), b"abd").unwrap();
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-badsha", "event": ev,
        "required_roles": [] })).await.unwrap_err();
    assert_eq!(err.code, "sha256_mismatch");
    assert_eq!(count(&db, "SELECT * FROM device_event").await, 0);
    assert_eq!(count(&db, "SELECT * FROM file_resource").await, 0);
    assert_eq!(count(&db, "SELECT * FROM has_attachment").await, 0);
}

#[tokio::test]
async fn commit_rejects_number_of_other_bundle_and_conflicting_retry() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    let a = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "b-a" })).await.unwrap();
    let pa = a["number"].as_str().unwrap().to_string();
    stage_standard(&e, "b-b", "U925000493", &pa, None).await;
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-b",
        "event": event(&pa, "STK", "PASS", "2026-10-02", json!("2027-10")) })).await.unwrap_err();
    assert_eq!(err.code, "protocol_no_taken");
    // An unreserved number is refused too.
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-b",
        "event": event("2026-0999", "STK", "PASS", "2026-10-02", json!("2027-10")) })).await.unwrap_err();
    assert_eq!(err.code, "protocol_not_reserved");

    // Own number: commit, then a retry with a different payload → conflict.
    let c = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "b-c" })).await.unwrap();
    let pc = c["number"].as_str().unwrap().to_string();
    stage_standard(&e, "b-c", "U925000493", &pc, None).await;
    let ev = event(&pc, "STK", "PASS", "2026-10-02", json!("2027-10"));
    run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-c", "event": ev.clone() })).await.unwrap();
    let mut ev2 = ev.clone();
    ev2["result"] = json!("FAIL");
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "b-c", "event": ev2 })).await.unwrap_err();
    assert_eq!(err.code, "conflict");
    assert_eq!(count(&db, "SELECT * FROM device_event").await, 1);
}

#[tokio::test]
async fn allocation_reuse_sequence_seed_and_non_writer() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    assert_eq!(seed_legacy(&db).await.unwrap(), 4);
    assert_eq!(seed_legacy(&db).await.unwrap(), 0, "seed idempotent");
    let a1 = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "n1", "kind": "STK" }))
        .await
        .unwrap();
    assert_eq!(a1["number"], json!("2026-0005"));
    assert_eq!(a1["reused"], json!(false));
    let again = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "n1" })).await.unwrap();
    assert_eq!(again["number"], json!("2026-0005"));
    assert_eq!(again["reused"], json!(true));
    let a2 = run_tool(&e, "protocol_number_allocate", &json!({ "year": "2026", "bundle_id": "n2" })).await.unwrap();
    assert_eq!(a2["number"], json!("2026-0006"));
    let y27 = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2027, "bundle_id": "n3" })).await.unwrap();
    assert_eq!(y27["number"], json!("2027-0001"));
    // Abandoned (aborted) reservations are never recycled; a lost counter row
    // is recovered from the reservations.
    db.query("DELETE protocol_counter").await.unwrap().check().unwrap();
    let a3 = run_tool(&e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "n4" })).await.unwrap();
    assert_eq!(a3["number"], json!("2026-0007"));

    let ro = env(&db, &base, false);
    let err = run_tool(&ro, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": "n5" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_writer");
}

#[tokio::test]
async fn legacy_number_adopted_once() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    seed_legacy(&db).await.unwrap();
    stage_standard(&e, "push-0004", "I824000067", "2026-0004", Some("31885")).await;
    run_tool(&e, "bundle_commit", &json!({ "bundle_id": "push-0004",
        "event": event("2026-0004", "STK", "PASS_WITH_NOTES", "2026-10-28", json!("2027-11")) }))
        .await
        .unwrap();
    let r = load_row(&db, "protocol_reservation", "2026-0004").await.unwrap().unwrap();
    assert_eq!(r["state"], json!("committed"));
    assert_eq!(r["bundle_id"], json!("push-0004"));
    assert_eq!(count(&db, "SELECT * FROM mcp_write_audit WHERE string::starts_with(outcome, 'legacy_adopted')").await, 1);
    // Another bundle can no longer use it.
    stage_standard(&e, "push-0004b", "I824000067", "2026-0004", None).await;
    let err = run_tool(&e, "bundle_commit", &json!({ "bundle_id": "push-0004b",
        "event": event("2026-0004", "MTK", "PASS", "2026-10-28", json!("2027-11")) }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "protocol_no_taken");
}

#[tokio::test]
async fn device_history_events_and_next_due_per_kind() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    let serial = "S32000022";
    let commit = |bundle: &'static str, kind: &'static str, result: &'static str, date: &'static str,
                      due: Value, supersedes: Option<String>, revision: i64| {
        let e = &e;
        async move {
            let n = run_tool(e, "protocol_number_allocate", &json!({ "year": 2026, "bundle_id": bundle }))
                .await
                .unwrap()["number"]
                .as_str()
                .unwrap()
                .to_string();
            run_tool(e, "bundle_begin", &json!({ "bundle_id": bundle, "serial": serial, "manifest": {} }))
                .await
                .unwrap();
            upload(e, bundle, &F::new("other", "note.txt", bundle.as_bytes()), 1, &[0]).await.unwrap();
            let mut ev = event(&n, kind, result, date, due);
            ev["revision"] = json!(revision);
            ev["supersedes_protocol_no"] = json!(supersedes);
            run_tool(e, "bundle_commit", &json!({ "bundle_id": bundle, "event": ev, "required_roles": [] }))
                .await
                .unwrap();
            n
        }
    };
    let stk1 = commit("h1", "STK", "PASS", "2025-10-01", json!("2026-10"), None, 0).await;
    let stk2 = commit("h2", "STK", "PASS", "2026-10-01", json!("2027-10"), None, 0).await;
    // Correction of stk2 (revision 1) supersedes it and carries the new date.
    commit("h3", "STK", "PASS_WITH_NOTES", "2026-10-01", json!("2027-11"), Some(stk2.clone()), 1).await;
    commit("h4", "MTK", "PASS", "2026-03-01", json!("2028-03"), None, 0).await;
    // A later failed MTK does not move the MTK due date; a repair has its own kind.
    commit("h5", "MTK", "FAIL", "2026-09-01", Value::Null, None, 0).await;
    commit("h6", "REPAIR", "PASS", "2026-10-05", Value::Null, None, 0).await;
    let _ = stk1;

    let h = device_history_ext(&db, "s32000022").await.unwrap();
    assert_eq!(h["device"]["serial_norm"], json!(serial));
    let evs = h["events"].as_array().unwrap();
    assert_eq!(evs.len(), 6);
    assert_eq!(evs[0]["date"], json!("2026-10-05"), "newest first");
    assert_eq!(evs[0]["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(evs[0]["attachments"][0]["role"], json!("other"));
    assert_eq!(h["next_due"]["STK"], json!("2027-11"));
    assert_eq!(h["next_due"]["MTK"], json!("2028-03"));
    assert_eq!(h["next_due"]["REPAIR"], Value::Null);
    assert_eq!(device_history_ext(&db, "NOPE1").await.unwrap()["events"], json!([]));
}

#[tokio::test]
async fn gc_aborts_expired_open_staging() {
    let db = test_db().await;
    let base = temp_base();
    let e = env(&db, &base, true);
    run_tool(&e, "bundle_begin", &json!({ "bundle_id": "old", "serial": "F1", "manifest": {} })).await.unwrap();
    upload(&e, "old", &F::new("photo", "p.jpg", &[1, 2, 3]), 1, &[0]).await.unwrap();
    assert!(staging_dir(&base, "old").exists());
    db.query("UPDATE type::record('bundle_staging', 'old') SET expires_at = '2020-01-01T00:00:00Z'")
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(gc_expired(&db, &base).await.unwrap(), 1);
    assert!(!staging_dir(&base, "old").exists());
    let st = load_row(&db, "bundle_staging", "old").await.unwrap().unwrap();
    assert_eq!(st["state"], json!("aborted"));
    let err = run_tool(&e, "bundle_begin", &json!({ "bundle_id": "old", "serial": "F1", "manifest": {} }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "aborted");
}
