use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Row, params};
use serde::Serialize;

use crate::error::AppError;

/// Older interactions are deleted so history stays short.
const MAX_STORED: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    #[cfg_attr(test, ts(type = "number"))]
    pub id: i64,
    /// Milliseconds since the Unix epoch.
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    pub request: String,
    pub response: String,
    pub results: Vec<String>,
    pub awaiting_confirmation: bool,
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

pub fn insert(
    conn: &Connection,
    request: &str,
    response: &str,
    results: &[String],
    awaiting_confirmation: bool,
) -> Result<Interaction, AppError> {
    let created_at = now_millis();
    conn.execute(
        "INSERT INTO interactions (created_at, request, response, results, awaiting_confirmation)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            created_at,
            request,
            response,
            encode(results),
            awaiting_confirmation
        ],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM interactions WHERE id <= ?1 - ?2",
        params![id, MAX_STORED],
    )?;
    Ok(Interaction {
        id,
        created_at,
        request: request.to_owned(),
        response: response.to_owned(),
        results: results.to_vec(),
        awaiting_confirmation,
    })
}

/// Records the outcome of a confirmation prompt.
pub fn resolve(
    conn: &Connection,
    id: i64,
    response: &str,
    results: &[String],
) -> Result<Interaction, AppError> {
    conn.execute(
        "UPDATE interactions SET response = ?2, results = ?3, awaiting_confirmation = 0
         WHERE id = ?1",
        params![id, response, encode(results)],
    )?;
    Ok(conn.query_row("SELECT * FROM interactions WHERE id = ?1", [id], from_row)?)
}

/// Most recent interactions, oldest first.
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<Interaction>, AppError> {
    let mut statement = conn.prepare(
        "SELECT * FROM (SELECT * FROM interactions ORDER BY id DESC LIMIT ?1) ORDER BY id",
    )?;
    let rows = statement.query_map([limit as i64], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn clear(conn: &Connection) -> Result<(), AppError> {
    conn.execute("DELETE FROM interactions", [])?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Interaction> {
    let results: String = row.get("results")?;
    Ok(Interaction {
        id: row.get("id")?,
        created_at: row.get("created_at")?,
        request: row.get("request")?,
        response: row.get("response")?,
        results: serde_json::from_str(&results).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
        })?,
        awaiting_confirmation: row.get("awaiting_confirmation")?,
    })
}

fn encode(results: &[String]) -> String {
    serde_json::Value::from(results).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn stores_and_lists_interactions_in_order() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, "first", "one", &[], false).unwrap();
        insert(
            &conn,
            "second",
            "two",
            &["Turned off kitchen.".into()],
            false,
        )
        .unwrap();
        let recent = recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].request, "first");
        assert_eq!(recent[1].results, ["Turned off kitchen."]);
        assert_eq!(super::recent(&conn, 1).unwrap()[0].request, "second");
    }

    #[test]
    fn resolving_a_confirmation_updates_the_interaction() {
        let conn = db::open_in_memory().unwrap();
        let pending = insert(&conn, "unlock the door", "Unlock front door?", &[], true).unwrap();
        let resolved = resolve(&conn, pending.id, "Unlocked front door.", &[]).unwrap();
        assert!(!resolved.awaiting_confirmation);
        assert_eq!(resolved.response, "Unlocked front door.");
    }

    #[test]
    fn history_is_capped() {
        let conn = db::open_in_memory().unwrap();
        for index in 0..(MAX_STORED + 5) {
            insert(&conn, &index.to_string(), "", &[], false).unwrap();
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, MAX_STORED);
    }

    #[test]
    fn clear_removes_everything() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, "a", "b", &[], false).unwrap();
        clear(&conn).unwrap();
        assert!(recent(&conn, 10).unwrap().is_empty());
    }
}
