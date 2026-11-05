//! SQLite setup shared by the catalog and the job store.

use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// Opens `path` (an in-memory database when `None`) and creates the schema.
///
/// A database written with another `schema_version` is rebuilt: both stores hold data that can be
/// recreated or that an older layout cannot resume.
pub fn open(
    path: Option<&Path>,
    mmap_bytes: u64,
    schema_version: i64,
    schema: &str,
) -> rusqlite::Result<Connection> {
    let conn = match path {
        Some(path) => {
            let conn = Connection::open(path)?;
            conn.execute_batch(&format!(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA cache_size=-8192;
                 PRAGMA mmap_size={mmap_bytes};"
            ))?;
            conn
        }
        None => {
            let conn = Connection::open_in_memory()?;
            conn.execute_batch("PRAGMA cache_size=-2048;")?;
            conn
        }
    };
    conn.execute_batch("PRAGMA temp_store=MEMORY;")?;
    conn.busy_timeout(Duration::from_secs(5))?;

    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != schema_version {
        drop_all_tables(&conn)?;
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(schema)?;
    conn.pragma_update(None, "user_version", schema_version)?;
    Ok(conn)
}

/// Foreign keys are off during the drop so table order does not matter.
fn drop_all_tables(conn: &Connection) -> rusqlite::Result<()> {
    let tables = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    for table in tables {
        conn.execute_batch(&format!("DROP TABLE \"{}\";", table.replace('"', "\"\"")))?;
    }
    Ok(())
}
