//! Shelling out to the real `pg_restore`/`psql` binaries. Reimplementing
//! either dump format is out of scope; what this tool adds is running them
//! against a disposable database and turning "the exit code and some
//! stderr" into a clear verdict.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::format::DumpFormat;

/// The outcome of one restore attempt: whether the restoring program
/// itself exited zero, plus enough of its output to explain why not.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub program: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Restore `dump_path` (already known to be `format`) into whatever
/// database `restore_conn` points at, using the appropriate real binary.
pub fn restore(
    format: DumpFormat,
    dump_path: &Path,
    restore_conn: &str,
    pg_restore_bin: &str,
    psql_bin: &str,
) -> Result<RestoreOutcome> {
    if format.needs_pg_restore() {
        run_pg_restore(pg_restore_bin, dump_path, restore_conn)
    } else {
        run_psql(psql_bin, dump_path, restore_conn)
    }
}

fn run_pg_restore(bin: &str, dump_path: &Path, restore_conn: &str) -> Result<RestoreOutcome> {
    let output = Command::new(bin)
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("--dbname")
        .arg(restore_conn)
        .arg(dump_path)
        .output()
        .with_context(|| format!("running {bin} — is it installed and on PATH?"))?;

    Ok(RestoreOutcome {
        program: bin.to_string(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn run_psql(bin: &str, dump_path: &Path, restore_conn: &str) -> Result<RestoreOutcome> {
    let output = Command::new(bin)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("--dbname")
        .arg(restore_conn)
        .arg("--file")
        .arg(dump_path)
        .output()
        .with_context(|| format!("running {bin} — is it installed and on PATH?"))?;

    Ok(RestoreOutcome {
        program: bin.to_string(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The last `n` non-empty lines of `text` — enough to show the actual
/// error without dumping an entire noisy `pg_restore` transcript.
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_returns_everything_when_shorter_than_n() {
        assert_eq!(tail_lines("a\nb", 5), "a\nb");
    }

    #[test]
    fn tail_lines_keeps_only_the_last_n() {
        assert_eq!(tail_lines("a\nb\nc\nd\ne", 2), "d\ne");
    }

    #[test]
    fn tail_lines_skips_blank_lines() {
        assert_eq!(tail_lines("a\n\n\nb\nc", 2), "b\nc");
    }

    #[test]
    fn tail_lines_of_empty_string_is_empty() {
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn tail_lines_zero_requested_is_empty() {
        assert_eq!(tail_lines("a\nb\nc", 0), "");
    }
}
