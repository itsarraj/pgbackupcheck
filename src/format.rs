//! Sniffing which of `pg_dump`'s output formats a file actually is, so the
//! right restore tool gets invoked. `pg_restore` refuses plain-text SQL
//! dumps outright ("input file appears to be a text format dump. Please
//! use psql.") and `psql -f` can't read the binary formats at all, so
//! guessing wrong isn't a cosmetic problem — it's the difference between a
//! real check and an immediate, useless failure.

use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// The four shapes `pg_dump` can produce (`-Fp`/`-Fc`/`-Ft`/`-Fd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpFormat {
    /// `-Fp` (the default): plain SQL text, restored with `psql -f`.
    Plain,
    /// `-Fc`: `pg_restore`'s custom binary format, magic bytes `PGDMP`.
    Custom,
    /// `-Ft`: a POSIX tar archive of the same custom-format pieces.
    Tar,
    /// `-Fd`: a directory of per-table files plus a `toc.dat`.
    Directory,
}

impl DumpFormat {
    pub fn label(self) -> &'static str {
        match self {
            DumpFormat::Plain => "plain (SQL text)",
            DumpFormat::Custom => "custom",
            DumpFormat::Tar => "tar",
            DumpFormat::Directory => "directory",
        }
    }

    /// Whether restoring this format goes through `pg_restore` (everything
    /// except plain SQL, which `pg_restore` explicitly rejects).
    pub fn needs_pg_restore(self) -> bool {
        !matches!(self, DumpFormat::Plain)
    }
}

const CUSTOM_MAGIC: &[u8; 5] = b"PGDMP";
// The ustar magic lives 257 bytes into the first tar header block, per the
// POSIX tar format — not at the start of the file.
const USTAR_OFFSET: usize = 257;
const USTAR_MAGIC: &[u8; 5] = b"ustar";

/// Detect the format of a dump at `path`. A directory is unambiguously
/// `-Fd`; a regular file is sniffed by its first bytes.
pub fn detect(path: &Path) -> Result<DumpFormat> {
    if path.is_dir() {
        // A `-Fd` dump directory always contains a toc.dat; check for it so
        // an arbitrary directory doesn't get misreported as a dump.
        if !path.join("toc.dat").exists() {
            anyhow::bail!(
                "{} is a directory but has no toc.dat — not a pg_dump directory-format archive",
                path.display()
            );
        }
        return Ok(DumpFormat::Directory);
    }

    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; USTAR_OFFSET + USTAR_MAGIC.len()];
    let n = file.read(&mut buf).context("reading dump header")?;
    buf.truncate(n);

    Ok(detect_bytes(&buf))
}

/// The pure byte-sniffing part of [`detect`], split out so it can be unit
/// tested without touching a filesystem.
pub fn detect_bytes(buf: &[u8]) -> DumpFormat {
    if buf.len() >= CUSTOM_MAGIC.len() && &buf[..CUSTOM_MAGIC.len()] == CUSTOM_MAGIC {
        return DumpFormat::Custom;
    }
    if buf.len() >= USTAR_OFFSET + USTAR_MAGIC.len()
        && &buf[USTAR_OFFSET..USTAR_OFFSET + USTAR_MAGIC.len()] == USTAR_MAGIC
    {
        return DumpFormat::Tar;
    }
    DumpFormat::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_custom_format_by_magic_bytes() {
        let mut buf = b"PGDMP".to_vec();
        buf.extend_from_slice(&[1, 20, 0, 0, 0]);
        assert_eq!(detect_bytes(&buf), DumpFormat::Custom);
    }

    #[test]
    fn detects_plain_sql_text() {
        let sql = b"--\n-- PostgreSQL database dump\n--\n\nSET statement_timeout = 0;\n";
        assert_eq!(detect_bytes(sql), DumpFormat::Plain);
    }

    #[test]
    fn detects_tar_format_by_ustar_magic_at_offset_257() {
        let mut buf = vec![0u8; USTAR_OFFSET];
        buf.extend_from_slice(b"ustar\x00");
        assert_eq!(detect_bytes(&buf), DumpFormat::Tar);
    }

    #[test]
    fn short_buffer_falls_back_to_plain_not_a_panic() {
        assert_eq!(detect_bytes(b""), DumpFormat::Plain);
        assert_eq!(detect_bytes(b"PG"), DumpFormat::Plain);
    }

    #[test]
    fn buffer_that_is_exactly_the_ustar_offset_is_plain() {
        // Regression guard for an off-by-one: a buffer that ends exactly at
        // USTAR_OFFSET has no magic bytes to read at all, and must not
        // panic on the slice index.
        let buf = vec![0u8; USTAR_OFFSET];
        assert_eq!(detect_bytes(&buf), DumpFormat::Plain);
    }

    #[test]
    fn empty_directory_without_toc_dat_is_rejected() {
        let dir = std::env::temp_dir().join(format!(
            "pgbackupcheck-fmt-test-{}-{}",
            std::process::id(),
            "empty"
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let err = detect(&dir).unwrap_err();
        assert!(err.to_string().contains("toc.dat"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_with_toc_dat_is_directory_format() {
        let dir = std::env::temp_dir().join(format!(
            "pgbackupcheck-fmt-test-{}-{}",
            std::process::id(),
            "withtoc"
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("toc.dat"), b"fake toc").unwrap();
        assert_eq!(detect(&dir).unwrap(), DumpFormat::Directory);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn needs_pg_restore_is_false_only_for_plain() {
        assert!(!DumpFormat::Plain.needs_pg_restore());
        assert!(DumpFormat::Custom.needs_pg_restore());
        assert!(DumpFormat::Tar.needs_pg_restore());
        assert!(DumpFormat::Directory.needs_pg_restore());
    }

    #[test]
    fn labels_are_human_readable() {
        assert_eq!(DumpFormat::Plain.label(), "plain (SQL text)");
        assert_eq!(DumpFormat::Custom.label(), "custom");
        assert_eq!(DumpFormat::Tar.label(), "tar");
        assert_eq!(DumpFormat::Directory.label(), "directory");
    }
}
