//! Swapping the database name in a connection string.
//!
//! `pgbackupcheck` is handed one connection string that has permission to
//! `CREATE DATABASE` (typically pointed at the `postgres` maintenance db),
//! and needs to hand `pg_restore`/`psql` a connection string to the fresh
//! scratch database it just created instead. Both accepted libpq shapes —
//! the URI form and the `key=value` form — are supported.

use anyhow::{Context, Result};

/// Replace the database name in `conn` with `new_db`, keeping every other
/// part (user, password, host, port, query params) untouched.
pub fn with_dbname(conn: &str, new_db: &str) -> Result<String> {
    let trimmed = conn.trim();
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        with_dbname_uri(trimmed, new_db)
    } else {
        Ok(with_dbname_keyword(trimmed, new_db))
    }
}

fn with_dbname_uri(conn: &str, new_db: &str) -> Result<String> {
    let (scheme, rest) = conn
        .split_once("://")
        .context("connection URI missing ://")?;

    // Split off a query string (?sslmode=... etc.) before touching the path.
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };

    // rest is now `[user[:pass]@]host[:port][/dbname]`. The path — if any —
    // is everything after the first '/' that comes after the authority.
    let (authority, _old_db) = match rest.split_once('/') {
        Some((a, d)) => (a, Some(d)),
        None => (rest, None),
    };

    let mut out = format!("{scheme}://{authority}/{new_db}");
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}

/// `key=value key=value ...` form. `dbname` is replaced if present,
/// otherwise appended.
fn with_dbname_keyword(conn: &str, new_db: &str) -> String {
    let mut found = false;
    let mut parts: Vec<String> = conn
        .split_whitespace()
        .map(|tok| {
            if let Some(rest) = tok.strip_prefix("dbname=") {
                found = true;
                let _ = rest;
                format!("dbname={new_db}")
            } else {
                tok.to_string()
            }
        })
        .collect();
    if !found {
        parts.push(format!("dbname={new_db}"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_form_replaces_trailing_dbname() {
        let out = with_dbname("postgres://user@localhost:5432/olddb", "newdb").unwrap();
        assert_eq!(out, "postgres://user@localhost:5432/newdb");
    }

    #[test]
    fn uri_form_with_no_dbname_appends_one() {
        let out = with_dbname("postgres://user@localhost:5432", "newdb").unwrap();
        assert_eq!(out, "postgres://user@localhost:5432/newdb");
    }

    #[test]
    fn uri_form_preserves_query_string() {
        let out = with_dbname("postgres://user@localhost/olddb?sslmode=require", "newdb").unwrap();
        assert_eq!(out, "postgres://user@localhost/newdb?sslmode=require");
    }

    #[test]
    fn uri_form_preserves_userinfo_with_password() {
        let out = with_dbname("postgres://user:pw@host:5432/olddb", "newdb").unwrap();
        assert_eq!(out, "postgres://user:pw@host:5432/newdb");
    }

    #[test]
    fn postgresql_scheme_is_also_accepted() {
        let out = with_dbname("postgresql://host/olddb", "newdb").unwrap();
        assert_eq!(out, "postgresql://host/newdb");
    }

    #[test]
    fn keyword_form_replaces_existing_dbname() {
        let out = with_dbname("host=localhost dbname=olddb port=5432", "newdb").unwrap();
        assert_eq!(out, "host=localhost dbname=newdb port=5432");
    }

    #[test]
    fn keyword_form_appends_dbname_when_absent() {
        let out = with_dbname("host=localhost port=5432", "newdb").unwrap();
        assert_eq!(out, "host=localhost port=5432 dbname=newdb");
    }

    #[test]
    fn keyword_form_is_used_when_uri_scheme_absent() {
        // Sanity check that a bare "host=... dbname=..." string never gets
        // routed through the URI parser, which would mangle it.
        let out = with_dbname("host=db.internal dbname=x", "y").unwrap();
        assert_eq!(out, "host=db.internal dbname=y");
    }

    #[test]
    fn uri_form_trims_surrounding_whitespace() {
        let out = with_dbname("  postgres://host/olddb  ", "newdb").unwrap();
        assert_eq!(out, "postgres://host/newdb");
    }
}
