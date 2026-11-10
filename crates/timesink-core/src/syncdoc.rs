//! The documents that are synced to CouchDB: one per computer per day.
//!
//! Each computer only ever writes its own documents, so two computers can never edit the
//! same document and CouchDB conflicts cannot happen. A document holds that day's rows:
//!
//! ```json
//! { "_id": "day:mangrove:2026-11-10", "type": "day", "device": "mangrove",
//!   "day": "2026-11-10", "schema": 1,
//!   "intervals": [[start, end, "app_class", "workspace"], ...],
//!   "listening": [[start, end, "player"], ...],
//!   "domains":   [[start, end, "github.com"], ...] }
//! ```
//!
//! Window titles are deliberately NOT part of a document: they never leave the computer
//! that recorded them. Days are UTC days, and a row belongs to the day it started in.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// Seconds in a day (days are UTC).
pub const DAY_SECS: i64 = 86_400;

/// The version of the document layout, so a later change can be told apart.
pub const DOC_SCHEMA: i64 = 1;

/// One day's rows from one computer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DayRows {
    /// Focus rows: (start, end, application class, workspace).
    pub intervals: Vec<(i64, i64, String, String)>,
    /// Listening rows: (start, end, player).
    pub listening: Vec<(i64, i64, String)>,
    /// Website rows: (start, end, site).
    pub domains: Vec<(i64, i64, String)>,
}

impl DayRows {
    /// True when there is nothing to upload for the day.
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty() && self.listening.is_empty() && self.domains.is_empty()
    }
}

/// The UTC day number (days since 1970-01-01) a timestamp falls in.
pub fn day_of(ts: i64) -> i64 {
    ts.div_euclid(DAY_SECS)
}

/// The calendar date of a day number as (year, month, day). Uses the standard
/// "civil from days" arithmetic, so no date library is needed here.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month, day)
}

/// The day number of a calendar date (the reverse of `civil_from_days`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A day number written as `YYYY-MM-DD`.
pub fn day_string(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Read a `YYYY-MM-DD` date back into a day number.
pub fn parse_day_string(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    // Reject dates like 2026-02-31 that do not round-trip.
    (day_string(day) == format!("{y:04}-{m:02}-{d:02}")).then_some(day)
}

/// The CouchDB document id for a computer's day: `day:<device>:<YYYY-MM-DD>`.
pub fn doc_id(device: &str, day: i64) -> String {
    format!("day:{device}:{}", day_string(day))
}

/// Split a document id back into (device, day). `None` for ids that are not day documents.
pub fn parse_doc_id(id: &str) -> Option<(String, i64)> {
    let rest = id.strip_prefix("day:")?;
    // The date is the last part, so a device name containing ':' still works.
    let (device, date) = rest.rsplit_once(':')?;
    if device.is_empty() {
        return None;
    }
    Some((device.to_string(), parse_day_string(date)?))
}

/// Build the document for one computer's day. `rev` is the revision CouchDB last gave us
/// for this document (needed to update it), if there is one.
pub fn build_doc(device: &str, day: i64, rows: &DayRows, rev: Option<&str>) -> Value {
    let mut doc = json!({
        "_id": doc_id(device, day),
        "type": "day",
        "device": device,
        "day": day_string(day),
        "schema": DOC_SCHEMA,
        "intervals": rows.intervals.iter()
            .map(|(s, e, app, ws)| json!([s, e, app, ws])).collect::<Vec<_>>(),
        "listening": rows.listening.iter()
            .map(|(s, e, p)| json!([s, e, p])).collect::<Vec<_>>(),
        "domains": rows.domains.iter()
            .map(|(s, e, d)| json!([s, e, d])).collect::<Vec<_>>(),
    });
    if let Some(rev) = rev {
        doc["_rev"] = json!(rev);
    }
    doc
}

/// Read one of the lists in a document as rows, checking each row's shape.
fn rows_of<T>(doc: &Value, key: &str, convert: impl Fn(&[Value]) -> Option<T>) -> Result<Vec<T>> {
    let Some(list) = doc.get(key) else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or_else(|| anyhow!("'{key}' is not a list"))?;
    list.iter()
        .map(|row| {
            row.as_array()
                .and_then(|cells| convert(cells))
                .ok_or_else(|| anyhow!("a row in '{key}' has the wrong shape"))
        })
        .collect()
}

/// Read a day document from CouchDB: (device, day, rows). A document that is not a day
/// document, or is malformed, is an error so the caller can skip it with a message.
pub fn parse_doc(doc: &Value) -> Result<(String, i64, DayRows)> {
    let id = doc["_id"].as_str().context("document has no _id")?;
    let (device, day) = parse_doc_id(id).ok_or_else(|| anyhow!("'{id}' is not a day document"))?;
    // The document's own fields must agree with its id.
    if doc["device"].as_str() != Some(device.as_str()) {
        return Err(anyhow!("'{id}' names a different device inside"));
    }
    let num = |v: &Value| v.as_i64();
    let text = |v: &Value| v.as_str().map(str::to_string);
    let rows = DayRows {
        intervals: rows_of(doc, "intervals", |c| match c {
            [s, e, a, w] => Some((num(s)?, num(e)?, text(a)?, text(w)?)),
            _ => None,
        })?,
        listening: rows_of(doc, "listening", |c| match c {
            [s, e, p] => Some((num(s)?, num(e)?, text(p)?)),
            _ => None,
        })?,
        domains: rows_of(doc, "domains", |c| match c {
            [s, e, d] => Some((num(s)?, num(e)?, text(d)?)),
            _ => None,
        })?,
    };
    Ok((device, day, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers_and_dates_round_trip() {
        assert_eq!(day_string(0), "1970-01-01");
        assert_eq!(day_string(20_767), "2026-11-10");
        assert_eq!(parse_day_string("2026-11-10"), Some(20_767));
        assert_eq!(parse_day_string("2024-02-29"), Some(19_782));
        // Not real dates.
        assert_eq!(parse_day_string("2026-02-30"), None);
        assert_eq!(parse_day_string("2026-13-01"), None);
        assert_eq!(parse_day_string("nonsense"), None);
        assert_eq!(parse_day_string("2026-11-10-01"), None);
        // Every day for a few years survives the round trip.
        for day in -800..25_000 {
            assert_eq!(parse_day_string(&day_string(day)), Some(day), "{day}");
        }
        // Days are UTC days, and negative times floor correctly.
        assert_eq!(day_of(86_399), 0);
        assert_eq!(day_of(86_400), 1);
        assert_eq!(day_of(-1), -1);
    }

    #[test]
    fn document_ids_name_the_device_and_day() {
        assert_eq!(doc_id("mangrove", 20_767), "day:mangrove:2026-11-10");
        assert_eq!(
            parse_doc_id("day:mangrove:2026-11-10"),
            Some(("mangrove".to_string(), 20_767))
        );
        // A device name may itself contain a colon.
        assert_eq!(
            parse_doc_id("day:odd:name:2026-11-10"),
            Some(("odd:name".to_string(), 20_767))
        );
        assert_eq!(parse_doc_id("day::2026-11-10"), None);
        assert_eq!(parse_doc_id("other:mangrove:2026-11-10"), None);
        assert_eq!(parse_doc_id("day:mangrove:soon"), None);
        assert_eq!(parse_doc_id("_design/x"), None);
    }

    fn sample() -> DayRows {
        DayRows {
            intervals: vec![(100, 160, "zen".into(), "1".into())],
            listening: vec![(110, 150, "spotify".into())],
            domains: vec![(120, 140, "github.com".into())],
        }
    }

    #[test]
    fn a_document_round_trips_and_never_holds_titles() {
        let doc = build_doc("mangrove", 20_767, &sample(), Some("3-abc"));
        assert_eq!(doc["_rev"], "3-abc");
        assert_eq!(doc["day"], "2026-11-10");
        // Intervals carry four cells: no title among them.
        assert_eq!(doc["intervals"][0].as_array().unwrap().len(), 4);
        assert!(!doc.to_string().contains("title"));
        let (device, day, rows) = parse_doc(&doc).unwrap();
        assert_eq!((device.as_str(), day), ("mangrove", 20_767));
        assert_eq!(rows, sample());
        // Without a revision there is no _rev field at all.
        assert!(build_doc("mangrove", 20_767, &sample(), None)
            .get("_rev")
            .is_none());
    }

    #[test]
    fn malformed_documents_are_rejected_not_half_applied() {
        let good = build_doc("box", 5, &sample(), None);
        let mut bad = good.clone();
        bad["intervals"] = json!([[1, 2, "zen"]]);
        assert!(parse_doc(&bad).is_err());
        let mut bad = good.clone();
        bad["intervals"] = json!("nope");
        assert!(parse_doc(&bad).is_err());
        let mut bad = good.clone();
        bad["listening"] = json!([[1, "x", "p"]]);
        assert!(parse_doc(&bad).is_err());
        // The device inside must match the id.
        let mut bad = good.clone();
        bad["device"] = json!("other");
        assert!(parse_doc(&bad).is_err());
        assert!(parse_doc(&json!({"_id": "something-else"})).is_err());
        // Missing lists are just empty.
        let mut sparse = good;
        sparse.as_object_mut().unwrap().remove("domains");
        assert!(parse_doc(&sparse).unwrap().2.domains.is_empty());
    }
}
