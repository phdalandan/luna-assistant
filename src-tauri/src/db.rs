use std::path::Path;

use rusqlite::Connection;

use crate::error::AppError;

/// Each entry upgrades the schema by one version. Never edit an existing entry.
const MIGRATIONS: &[&str] =
    &["CREATE TABLE settings (id INTEGER PRIMARY KEY CHECK (id = 1), data TEXT NOT NULL) STRICT;"];
const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

pub fn open(path: &Path) -> Result<Connection, AppError> {
    let mut conn = Connection::open(path)?;
    migrate(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<Connection, AppError> {
    let mut conn = Connection::open_in_memory()?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<(), AppError> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > LATEST_VERSION {
        return Err(AppError::UnsupportedDatabaseVersion {
            found: version,
            supported: LATEST_VERSION,
        });
    }
    if version == LATEST_VERSION {
        return Ok(());
    }

    let tx = conn.transaction()?;
    for migration in &MIGRATIONS[version as usize..] {
        tx.execute_batch(migration)?;
    }
    tx.pragma_update(None, "user_version", LATEST_VERSION)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{self, Settings};

    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("luna-{name}-{}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn settings_persist_across_reopen() {
        let db = TempDb::new("reopen");
        let saved = Settings {
            model: "gemma3:12b".into(),
            ..Settings::default()
        };
        settings::save(&open(&db.0).unwrap(), &saved).unwrap();
        assert_eq!(settings::load(&open(&db.0).unwrap()).unwrap(), saved);
    }

    #[test]
    fn migration_is_idempotent() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, LATEST_VERSION);
    }

    #[test]
    fn rejects_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();
        assert!(matches!(
            migrate(&mut conn),
            Err(AppError::UnsupportedDatabaseVersion { .. })
        ));
    }
}
