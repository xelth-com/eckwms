//! Monthly expense-report generator for the Fahrtenbuch.
//!
//! Aggregates ended `trip` rows for a calendar month, pulls the durable
//! `fuel_events` (Tankbeleg / Parkschein / Maut / receipt) attached to each
//! trip, and runs "forgotten items" gap detection (missing receipt photos,
//! interrupted recordings, un-fueled long days, unfiled months). Read-only
//! over `trip`; the only write is the operator's "marked as filed" flag in
//! `expense_report_status`.

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Months, NaiveDate};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ---- pure model (DB-free, so gap logic is unit-testable) -------------------

#[derive(Debug, Clone)]
pub struct ExpenseLite {
    pub kind: String,
    pub ts: String,
    pub photo_id: Option<String>,
    pub label: Option<String>,
    pub odometer_km: Option<f64>,
    pub trip_id: String,
}

#[derive(Debug, Clone)]
pub struct TripLite {
    pub id: String,
    pub date: String, // YYYY-MM-DD, derived from started_at
    pub started_at: String,
    pub ended_at: Option<String>,
    pub km: Option<f64>,
    pub computed_distance_km: Option<f64>,
    pub purpose: String,
    pub note: Option<String>,
    pub interrupted: bool,
    pub expenses: Vec<ExpenseLite>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub code: &'static str,
    pub severity: &'static str,
    pub date: String,
    pub detail: String,
}

impl Gap {
    fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "severity": self.severity,
            "date": self.date,
            "detail": self.detail,
        })
    }
}

/// Pure gap detection. `filed` = an `expense_report_status` record exists.
pub fn detect_gaps(trips: &[TripLite], filed: bool) -> Vec<Gap> {
    let mut gaps = Vec::new();

    for t in trips {
        // G1 — expense event with no receipt photo (Beleg-Foto fehlt).
        for e in &t.expenses {
            if e.photo_id.as_deref().unwrap_or("").is_empty() {
                gaps.push(Gap {
                    code: "G1",
                    severity: "warn",
                    date: t.date.clone(),
                    detail: format!(
                        "receipt photo missing for {} event at {} (Beleg-Foto fehlt)",
                        e.kind, e.ts
                    ),
                });
            }
        }

        // G2 — recording interrupted (auto-closed trip).
        if t.interrupted {
            gaps.push(Gap {
                code: "G2",
                severity: "warn",
                date: t.date.clone(),
                detail: "recording interrupted; receipts from this trip may be unrecorded"
                    .to_string(),
            });
        }

        // G3 — ended trip longer than 15 min with no computed distance.
        if let Some(mins) = trip_duration_minutes(t) {
            let no_distance = t.computed_distance_km.unwrap_or(0.0) == 0.0;
            if mins > 15.0 && no_distance {
                gaps.push(Gap {
                    code: "G3",
                    severity: "info",
                    date: t.date.clone(),
                    detail: format!(
                        "trip ran {:.0} min but distance was not computed",
                        mins
                    ),
                });
            }
        }
    }

    // G4 — a day summing >300 km with no fuel event on that day or the next.
    let mut km_by_day: BTreeMap<String, f64> = BTreeMap::new();
    let mut fuel_days: HashSet<String> = HashSet::new();
    for t in trips {
        *km_by_day.entry(t.date.clone()).or_insert(0.0) += t.km.unwrap_or(0.0);
        if t.expenses.iter().any(|e| e.kind == "fuel") {
            fuel_days.insert(t.date.clone());
        }
    }
    for (day, km) in &km_by_day {
        if *km > 300.0 {
            let next = next_day(day);
            let fueled = fuel_days.contains(day)
                || next.as_deref().is_some_and(|n| fuel_days.contains(n));
            if !fueled {
                gaps.push(Gap {
                    code: "G4",
                    severity: "info",
                    date: day.clone(),
                    detail: format!(
                        "{:.0} km driven with no fuel receipt on this day or the next",
                        km
                    ),
                });
            }
        }
    }

    // G5 — business trips present but the month is not marked as filed.
    let has_business = trips.iter().any(|t| t.purpose == "business");
    if has_business && !filed {
        gaps.push(Gap {
            code: "G5",
            severity: "info",
            date: String::new(),
            detail: "expense report for this month not marked as filed".to_string(),
        });
    }

    gaps
}

fn trip_duration_minutes(t: &TripLite) -> Option<f64> {
    let start = DateTime::parse_from_rfc3339(&t.started_at).ok()?;
    let end = DateTime::parse_from_rfc3339(t.ended_at.as_deref()?).ok()?;
    Some((end - start).num_seconds() as f64 / 60.0)
}

fn next_day(day: &str) -> Option<String> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(d.succ_opt()?.format("%Y-%m-%d").to_string())
}

// ---- DB → model ------------------------------------------------------------

fn opt_f64(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}

fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn row_to_trip(row: &Value) -> TripLite {
    let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let started_at = row
        .get("started_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let date = started_at.get(0..10).unwrap_or("").to_string();
    let note = opt_str(row, "note");
    let interrupted = note
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .contains("auto-closed");
    // Best available distance: computed → map-matched → smoothed estimate.
    let computed = opt_f64(row, "computed_distance_km");
    let km = computed
        .or_else(|| opt_f64(row, "matched_km"))
        .or_else(|| opt_f64(row, "smoothed_km"));

    let expenses = row
        .get("fuel_events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|e| ExpenseLite {
                    kind: e.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    ts: e.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    photo_id: opt_str(e, "photo_id"),
                    label: opt_str(e, "label"),
                    odometer_km: opt_f64(e, "odometer_km"),
                    trip_id: id.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    TripLite {
        id,
        date,
        started_at,
        ended_at: opt_str(row, "ended_at"),
        km,
        computed_distance_km: computed,
        purpose: row
            .get("purpose")
            .and_then(|v| v.as_str())
            .unwrap_or("business")
            .to_string(),
        note,
        interrupted,
        expenses,
    }
}

// ---- handlers --------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReportQuery {
    pub month: String, // YYYY-MM
    #[serde(default)]
    pub format: Option<String>, // "md" → text/markdown
}

/// GET /api/reports/expense?month=YYYY-MM[&format=md]
pub async fn get_expense_report(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReportQuery>,
) -> Result<Response, (StatusCode, String)> {
    let month = q.month.trim().to_string();
    // Month bounds — RFC3339 string compare works on the stored started_at.
    let first = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .map_err(|_| (StatusCode::BAD_REQUEST, "month must be YYYY-MM".to_string()))?;
    let next = first
        .checked_add_months(Months::new(1))
        .ok_or((StatusCode::BAD_REQUEST, "month out of range".to_string()))?;
    let start = format!("{month}-01T00:00:00Z");
    let next_start = next.format("%Y-%m-%dT00:00:00Z").to_string();

    let rows: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, started_at, ended_at, computed_distance_km, \
             matched_km, smoothed_km, purpose, note, fuel_events \
             FROM trip \
             WHERE started_at >= $start AND started_at < $next AND status != 'open' \
             ORDER BY started_at ASC LIMIT 2000",
        )
        .bind(("start", start))
        .bind(("next", next_start))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let trips: Vec<TripLite> = rows.iter().map(row_to_trip).collect();

    // Filed status for this month (drives G5).
    let filed_rows: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, month, filed_at, reference, note \
             FROM type::record('expense_report_status', $m)",
        )
        .bind(("m", month.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();
    let filed = filed_rows.into_iter().next();

    let gaps = detect_gaps(&trips, filed.is_some());
    let report = build_report(&month, &trips, filed.as_ref(), &gaps);

    if q.format.as_deref() == Some("md") {
        let md = render_markdown(&month, &trips, filed.as_ref(), &gaps);
        Ok((
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            md,
        )
            .into_response())
    } else {
        Ok(Json(report).into_response())
    }
}

fn build_report(
    month: &str,
    trips: &[TripLite],
    filed: Option<&Value>,
    gaps: &[Gap],
) -> Value {
    // Group by day, preserving started_at order.
    let mut by_day: BTreeMap<String, Vec<&TripLite>> = BTreeMap::new();
    for t in trips {
        by_day.entry(t.date.clone()).or_default().push(t);
    }

    let mut total_km = 0.0;
    let mut expense_events = 0usize;
    let mut events_with_photo = 0usize;

    let days: Vec<Value> = by_day
        .iter()
        .map(|(date, day_trips)| {
            let trips_json: Vec<Value> = day_trips
                .iter()
                .map(|t| {
                    total_km += t.km.unwrap_or(0.0);
                    json!({
                        "id": t.id,
                        "started_at": t.started_at,
                        "ended_at": t.ended_at,
                        "km": t.km,
                        "purpose": t.purpose,
                        "note": t.note,
                        "interrupted": t.interrupted,
                    })
                })
                .collect();
            let expenses_json: Vec<Value> = day_trips
                .iter()
                .flat_map(|t| t.expenses.iter())
                .map(|e| {
                    expense_events += 1;
                    if e.photo_id.is_some() {
                        events_with_photo += 1;
                    }
                    json!({
                        "kind": e.kind,
                        "ts": e.ts,
                        "photo_id": e.photo_id,
                        "label": e.label,
                        "odometer_km": e.odometer_km,
                        "trip_id": e.trip_id,
                    })
                })
                .collect();
            json!({ "date": date, "trips": trips_json, "expenses": expenses_json })
        })
        .collect();

    let filed_json = filed.map(|f| {
        json!({
            "filed_at": f.get("filed_at").cloned().unwrap_or(Value::Null),
            "reference": f.get("reference").cloned().unwrap_or(Value::Null),
        })
    });

    json!({
        "month": month,
        "days": days,
        "totals": {
            "trips": trips.len(),
            "km": total_km,
            "expense_events": expense_events,
            "events_with_photo": events_with_photo,
        },
        "filed": filed_json,
        "gaps": gaps.iter().map(|g| g.to_json()).collect::<Vec<_>>(),
    })
}

fn render_markdown(
    month: &str,
    trips: &[TripLite],
    filed: Option<&Value>,
    gaps: &[Gap],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Expense report {month}\n\n"));

    let filed_line = match filed {
        Some(f) => format!(
            "**Filed:** {} (ref {})\n\n",
            f.get("filed_at").and_then(|v| v.as_str()).unwrap_or("?"),
            f.get("reference").and_then(|v| v.as_str()).unwrap_or("?"),
        ),
        None => "**Filed:** no\n\n".to_string(),
    };
    out.push_str(&filed_line);

    let mut by_day: BTreeMap<String, Vec<&TripLite>> = BTreeMap::new();
    for t in trips {
        by_day.entry(t.date.clone()).or_default().push(t);
    }

    let mut total_km = 0.0;
    for (date, day_trips) in &by_day {
        out.push_str(&format!("## {date}\n\n"));
        out.push_str("| trip | km | purpose | expenses | note |\n");
        out.push_str("|---|---|---|---|---|\n");
        for t in day_trips {
            let km = t.km.unwrap_or(0.0);
            total_km += km;
            let exp: Vec<String> = t
                .expenses
                .iter()
                .map(|e| {
                    let photo = if e.photo_id.is_some() { "" } else { " (no photo)" };
                    format!("{}{}", e.kind, photo)
                })
                .collect();
            out.push_str(&format!(
                "| {} | {:.1} | {} | {} | {} |\n",
                &t.id.chars().take(8).collect::<String>(),
                km,
                t.purpose,
                if exp.is_empty() { "-".to_string() } else { exp.join(", ") },
                t.note.as_deref().unwrap_or(""),
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "**Totals:** {} trips, {:.1} km\n\n",
        trips.len(),
        total_km
    ));

    out.push_str("## Gaps\n\n");
    if gaps.is_empty() {
        out.push_str("None detected.\n");
    } else {
        for g in gaps {
            let d = if g.date.is_empty() {
                String::new()
            } else {
                format!(" [{}]", g.date)
            };
            out.push_str(&format!(
                "- **{}** ({}){}: {}\n",
                g.code, g.severity, d, g.detail
            ));
        }
    }
    out
}

#[derive(Deserialize)]
pub struct FiledBody {
    pub reference: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// POST /api/reports/expense/:month/filed — mark the month as filed.
pub async fn set_filed(
    State(state): State<Arc<AppState>>,
    Path(month): Path<String>,
    Json(body): Json<FiledBody>,
) -> ApiResult<Json<Value>> {
    // CONTENT fully replaces the row → no greedy SET/WHERE risk. `.check()`
    // surfaces a statement-level error (bad cast / parse).
    let rows: Vec<Value> = state
        .db
        .query(
            "UPSERT type::record('expense_report_status', $m) CONTENT { \
             month: $m, filed_at: time::now(), reference: $ref, note: $note } \
             RETURN record::id(id) AS id, month, filed_at, reference, note",
        )
        .bind(("m", month.clone()))
        .bind(("ref", body.reference))
        .bind(("note", body.note))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    Ok(Json(json!({
        "success": true,
        "status": rows.into_iter().next().unwrap_or(Value::Null),
    })))
}

/// DELETE /api/reports/expense/:month/filed — clear the filed flag.
pub async fn clear_filed(
    State(state): State<Arc<AppState>>,
    Path(month): Path<String>,
) -> ApiResult<Json<Value>> {
    state
        .db
        .query("DELETE type::record('expense_report_status', $m)")
        .bind(("m", month))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;

    Ok(Json(json!({ "success": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trip(id: &str, date: &str, purpose: &str) -> TripLite {
        TripLite {
            id: id.to_string(),
            date: date.to_string(),
            started_at: format!("{date}T08:00:00Z"),
            ended_at: Some(format!("{date}T09:00:00Z")),
            km: Some(50.0),
            computed_distance_km: Some(50.0),
            purpose: purpose.to_string(),
            note: None,
            interrupted: false,
            expenses: vec![],
        }
    }

    fn expense(kind: &str, photo: Option<&str>) -> ExpenseLite {
        ExpenseLite {
            kind: kind.to_string(),
            ts: "2026-07-02T08:30:00Z".to_string(),
            photo_id: photo.map(|s| s.to_string()),
            label: None,
            odometer_km: None,
            trip_id: "t1".to_string(),
        }
    }

    #[test]
    fn gaps_g1_g2_g5() {
        let mut t = trip("t1", "2026-07-02", "business");
        t.interrupted = true;
        t.note = Some("auto-closed after signal loss".to_string());
        t.expenses = vec![expense("fuel", None), expense("toll", Some("cas123"))];

        let gaps = detect_gaps(&[t], false);
        let codes: Vec<&str> = gaps.iter().map(|g| g.code).collect();
        // Missing-photo fuel event → G1; interrupted → G2; unfiled business → G5.
        assert!(codes.contains(&"G1"), "expected G1, got {codes:?}");
        assert!(codes.contains(&"G2"), "expected G2, got {codes:?}");
        assert!(codes.contains(&"G5"), "expected G5, got {codes:?}");
        // The toll event has a photo → exactly one G1.
        assert_eq!(gaps.iter().filter(|g| g.code == "G1").count(), 1);
    }

    #[test]
    fn gaps_g3_g4_and_filed_suppresses_g5() {
        // Long day, >300 km, no fuel event anywhere → G4; also a zero-distance
        // long trip → G3.
        let mut a = trip("a", "2026-07-05", "business");
        a.km = Some(200.0);
        a.computed_distance_km = Some(0.0); // ended, 60 min, no distance → G3
        let mut b = trip("b", "2026-07-05", "business");
        b.km = Some(150.0);
        b.computed_distance_km = Some(150.0);

        let gaps = detect_gaps(&[a, b], true); // filed → no G5
        let codes: Vec<&str> = gaps.iter().map(|g| g.code).collect();
        assert!(codes.contains(&"G3"), "expected G3, got {codes:?}");
        assert!(codes.contains(&"G4"), "expected G4, got {codes:?}");
        assert!(!codes.contains(&"G5"), "G5 must be suppressed when filed");
    }
}
