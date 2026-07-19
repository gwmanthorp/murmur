use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::settings::dpapi;

const RETENTION_LIMIT: i64 = 20;
const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct HistoryStore {
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at_ms: i64,
    pub text: String,
}

impl HistoryStore {
    pub fn initialize(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create the history folder: {error}"))?;
        }
        let store = Self { path };
        store.initialize_schema()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, String> {
        Connection::open(&self.path).map_err(|error| format!("Could not open History: {error}"))
    }

    fn initialize_schema(&self) -> Result<(), String> {
        let connection = self.connect()?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("Could not read the History version: {error}"))?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "History database version {version} is not supported"
            ));
        }
        connection
            .execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS history_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at_ms INTEGER NOT NULL,
                    text_dpapi TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_history_created
                    ON history_entries(created_at_ms DESC, id DESC);
                 COMMIT;",
            )
            .map_err(|error| format!("Could not initialize History: {error}"))?;
        if version == 0 {
            connection
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|error| format!("Could not version History: {error}"))?;
        }
        Ok(())
    }

    pub fn insert(&self, text: &str) -> Result<i64, String> {
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("Could not timestamp the dictation: {error}"))?
            .as_millis() as i64;
        self.insert_at(text, created_at_ms)
    }

    fn insert_at(&self, text: &str, created_at_ms: i64) -> Result<i64, String> {
        if text.trim().is_empty() {
            return Err("Empty dictations are not saved to History".into());
        }
        let protected = dpapi::encrypt(text)
            .ok_or_else(|| "Windows could not protect the History entry".to_string())?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not update History: {error}"))?;
        transaction
            .execute(
                "INSERT INTO history_entries (created_at_ms, text_dpapi) VALUES (?1, ?2)",
                params![created_at_ms, protected],
            )
            .map_err(|error| format!("Could not save the dictation to History: {error}"))?;
        let id = transaction.last_insert_rowid();
        transaction
            .execute(
                "DELETE FROM history_entries
                 WHERE id NOT IN (
                    SELECT id FROM history_entries
                    ORDER BY created_at_ms DESC, id DESC LIMIT ?1
                 )",
                [RETENTION_LIMIT],
            )
            .map_err(|error| format!("Could not prune History: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit History: {error}"))?;
        Ok(id)
    }

    pub fn list(&self) -> Result<Vec<HistoryEntry>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT id, created_at_ms, text_dpapi FROM history_entries
                 ORDER BY created_at_ms DESC, id DESC LIMIT ?1",
            )
            .map_err(|error| format!("Could not read History: {error}"))?;
        let rows = statement
            .query_map([RETENTION_LIMIT], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("Could not read History: {error}"))?;

        let mut entries = Vec::new();
        let mut unreadable = 0usize;
        for row in rows {
            let (id, created_at_ms, protected) =
                row.map_err(|error| format!("Could not read a History entry: {error}"))?;
            if let Some(text) = dpapi::decrypt(&protected) {
                entries.push(HistoryEntry {
                    id,
                    created_at_ms,
                    text,
                });
            } else {
                unreadable += 1;
                tracing::warn!(id, "history entry could not be decrypted");
            }
        }
        if unreadable > 0 {
            return Err(format!(
                "{unreadable} History entr{} could not be decrypted for this Windows user",
                if unreadable == 1 { "y" } else { "ies" }
            ));
        }
        Ok(entries)
    }

    pub fn text(&self, id: i64) -> Result<String, String> {
        let connection = self.connect()?;
        let protected = connection
            .query_row(
                "SELECT text_dpapi FROM history_entries WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    "That History entry no longer exists".to_string()
                }
                _ => format!("Could not read the History entry: {error}"),
            })?;
        dpapi::decrypt(&protected)
            .ok_or_else(|| "Windows could not decrypt that History entry".to_string())
    }

    pub fn delete(&self, id: i64) -> Result<(), String> {
        let changed = self
            .connect()?
            .execute("DELETE FROM history_entries WHERE id = ?1", [id])
            .map_err(|error| format!("Could not delete the History entry: {error}"))?;
        if changed == 0 {
            return Err("That History entry no longer exists".into());
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), String> {
        self.connect()?
            .execute("DELETE FROM history_entries", [])
            .map_err(|error| format!("Could not clear History: {error}"))?;
        Ok(())
    }

    #[cfg(test)]
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> HistoryStore {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        HistoryStore::initialize(std::env::temp_dir().join(format!(
            "murmur-history-test-{}-{stamp}.db",
            std::process::id()
        )))
        .unwrap()
    }

    fn cleanup(store: &HistoryStore) {
        let _ = std::fs::remove_file(store.path());
    }

    #[test]
    fn encrypted_entries_round_trip_and_reopen() {
        let store = test_store();
        store.insert_at("A private dictation.", 123).unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("A private dictation."));
        let reopened = HistoryStore::initialize(store.path().to_path_buf()).unwrap();
        assert_eq!(
            reopened.list().unwrap(),
            vec![HistoryEntry {
                id: 1,
                created_at_ms: 123,
                text: "A private dictation.".into(),
            }]
        );
        cleanup(&store);
    }

    #[test]
    fn newest_twenty_are_retained_in_order() {
        let store = test_store();
        for index in 0..21 {
            store.insert_at(&format!("entry {index}"), index).unwrap();
        }
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 20);
        assert_eq!(entries.first().unwrap().text, "entry 20");
        assert_eq!(entries.last().unwrap().text, "entry 1");
        cleanup(&store);
    }

    #[test]
    fn delete_and_clear_are_persistent() {
        let store = test_store();
        let first = store.insert_at("first", 1).unwrap();
        store.insert_at("second", 2).unwrap();
        store.delete(first).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        store.clear().unwrap();
        assert!(store.list().unwrap().is_empty());
        cleanup(&store);
    }

    #[test]
    fn corrupt_ciphertext_returns_an_actionable_error() {
        let store = test_store();
        store
            .connect()
            .unwrap()
            .execute(
                "INSERT INTO history_entries (created_at_ms, text_dpapi) VALUES (1, 'broken')",
                [],
            )
            .unwrap();
        assert!(store.list().unwrap_err().contains("could not be decrypted"));
        cleanup(&store);
    }
}
