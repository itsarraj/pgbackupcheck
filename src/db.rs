//! The only parts that touch Postgres directly over the wire: creating and
//! dropping the scratch database, and counting what actually landed in it
//! after a restore. Everything else (the restore itself) is delegated to
//! the real `pg_restore`/`psql` binaries in `runner.rs` — reimplementing
//! either of those in Rust is a non-goal.

use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};

/// A valid, unquoted-safe Postgres identifier: this is deliberately
/// stricter than what Postgres itself allows (which would require quoting
/// and escaping to embed safely in DDL we build as plain strings).
pub fn validate_identifier(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if name.is_empty() || name.len() > 63 || !first_ok || !rest_ok {
        bail!(
            "'{name}' is not a safe database identifier (must start with a letter or \
             underscore, contain only letters/digits/underscores, and be 1-63 characters)"
        );
    }
    Ok(())
}

pub fn connect(conninfo: &str) -> Result<Client> {
    Client::connect(conninfo, NoTls).context("connecting to the admin database")
}

/// `CREATE DATABASE` for the scratch restore target. Refuses to run if a
/// database with this name already exists, so a re-run never restores into
/// (and silently reports on) leftovers from a previous crashed run.
pub fn create_scratch_db(client: &mut Client, name: &str) -> Result<()> {
    validate_identifier(name)?;
    let exists: bool = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            &[&name],
        )
        .context("checking whether the scratch database already exists")?
        .get(0);
    if exists {
        bail!(
            "database \"{name}\" already exists — pick a different --scratch-db, \
             or drop it first if it's leftover from a previous run"
        );
    }
    // TEMPLATE template0 with an explicit, always-available 'C' locale
    // rather than the server default `CREATE DATABASE "{name}"` would
    // silently copy from template1: a scratch database created only to
    // test-restore a dump has no business inheriting whatever locale,
    // encoding, or extensions someone bolted onto this server's template1
    // over the years. It also happens to sidestep a real footgun found
    // while building this — a server whose template1 has a stale collation
    // provider version (common after an OS library upgrade) rejects a
    // plain `CREATE DATABASE` outright until someone runs `ALTER DATABASE
    // template1 REFRESH COLLATION VERSION`; going through template0 avoids
    // depending on that having been done.
    client
        .batch_execute(&format!(
            "CREATE DATABASE \"{name}\" TEMPLATE template0 ENCODING 'UTF8' \
             LC_COLLATE 'C' LC_CTYPE 'C'"
        ))
        .with_context(|| format!("creating scratch database \"{name}\""))?;
    Ok(())
}

/// `DROP DATABASE`, forcibly disconnecting any lingering session first
/// (Postgres 13+ `WITH (FORCE)`; older servers fall back to a plain drop,
/// which fails loudly if something is still connected rather than hanging).
pub fn drop_scratch_db(client: &mut Client, name: &str) -> Result<()> {
    validate_identifier(name)?;
    let forced = client.batch_execute(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"));
    if forced.is_err() {
        client
            .batch_execute(&format!("DROP DATABASE \"{name}\""))
            .with_context(|| format!("dropping scratch database \"{name}\""))?;
    }
    Ok(())
}

/// What actually ended up in the restored database: table count in the
/// `public` schema, and — only when asked for, since it means a full
/// `count(*)` per table — the summed row count across all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredContents {
    pub table_count: i64,
    pub total_rows: Option<i64>,
}

pub fn inspect_restored_db(client: &mut Client, count_rows: bool) -> Result<RestoredContents> {
    let tables: Vec<String> = client
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
            &[],
        )
        .context("listing restored tables")?
        .iter()
        .map(|row| row.get(0))
        .collect();

    let total_rows = if count_rows {
        let mut sum = 0i64;
        for table in &tables {
            // Table names above came straight from the catalog, so this
            // isn't attacker-controlled input, but it's still identifier
            // syntax, not a literal — quote it rather than trust it's
            // simple.
            let quoted = table.replace('"', "\"\"");
            let row = client
                .query_one(&format!("SELECT count(*) FROM \"{quoted}\""), &[])
                .with_context(|| format!("counting rows in \"{table}\""))?;
            let count: i64 = row.get(0);
            sum += count;
        }
        Some(sum)
    } else {
        None
    };

    Ok(RestoredContents {
        table_count: tables.len() as i64,
        total_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_identifiers() {
        assert!(validate_identifier("pgbackupcheck_1758300000_42").is_ok());
        assert!(validate_identifier("_leading_underscore").is_ok());
        assert!(validate_identifier("a").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn rejects_leading_digit() {
        assert!(validate_identifier("1abc").is_err());
    }

    #[test]
    fn rejects_sql_injection_attempt() {
        assert!(validate_identifier("x\"; DROP TABLE users; --").is_err());
        assert!(validate_identifier("db name").is_err());
        assert!(validate_identifier("db;name").is_err());
    }

    #[test]
    fn rejects_over_63_chars() {
        let long = "a".repeat(64);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn accepts_exactly_63_chars() {
        let ok = "a".repeat(63);
        assert!(validate_identifier(&ok).is_ok());
    }
}
