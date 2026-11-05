use rusqlite::Connection;
use std::time::Duration;

pub(super) fn configure_connection(
    conn: &Connection,
    use_wal: bool,
) -> Result<(), rusqlite::Error> {
    if use_wal {
        // A small mmap (32 MB) saves virtual memory; hot pages are already in the cache_size page cache.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-8192;
             PRAGMA mmap_size=33554432;
             PRAGMA foreign_keys=ON;",
        )?;
    } else {
        conn.execute_batch(
            "PRAGMA temp_store=MEMORY;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-2048;
             PRAGMA foreign_keys=ON;",
        )?;
    }

    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}
