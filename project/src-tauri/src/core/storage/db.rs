use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};

use super::migrations::run_migrations;
use crate::core::intent_builtin_catalog::ensure_builtin_catalog;

pub fn open_write(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = FULL;
        PRAGMA cache_size = -8000;
        PRAGMA foreign_keys = ON;
    "#,
    )?;
    run_migrations(&mut conn)?;
    ensure_builtin_catalog(&mut conn)?;
    Ok(conn)
}

pub fn open_read(path: &Path) -> rusqlite::Result<Arc<Mutex<Connection>>> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.execute_batch(
        r#"
        PRAGMA cache_size = -8000;
    "#,
    )?;
    Ok(Arc::new(Mutex::new(conn)))
}

pub fn wal_checkpoint(conn: &Connection) -> rusqlite::Result<()> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |_r| Ok(()))
}

pub fn wal_size_bytes(path: &Path) -> u64 {
    let wal = path.with_extension("sqlite-wal");
    std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
#[test]
fn read_connection_supports_a_literal_hash_in_the_database_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data#recovery.sqlite");
    let write = open_write(&path).unwrap();
    write.execute_batch("CREATE TABLE filename_probe(value TEXT); INSERT INTO filename_probe VALUES('ok');").unwrap();
    let read = open_read(&path).unwrap();
    let value: String = read.lock().query_row("SELECT value FROM filename_probe", [], |row| row.get(0)).unwrap();
    assert_eq!(value, "ok");
}
