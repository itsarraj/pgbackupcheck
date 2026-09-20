//! Turning a restore attempt into a verdict and a human-readable summary.

use crate::db::RestoredContents;
use crate::format::DumpFormat;
use crate::runner::{tail_lines, RestoreOutcome};

pub struct CheckReport {
    pub dump_path: String,
    pub format: DumpFormat,
    pub scratch_db: String,
    pub restore: RestoreOutcome,
    /// `None` when the restore itself failed, since there is nothing
    /// meaningful to inspect in that case.
    pub contents: Option<RestoredContents>,
    pub min_tables: i64,
    pub elapsed_secs: f64,
}

impl CheckReport {
    /// The single question this whole tool exists to answer: did the
    /// backup actually restore, and did it restore *something*.
    pub fn passed(&self) -> bool {
        self.restore.success
            && self
                .contents
                .as_ref()
                .is_some_and(|c| c.table_count >= self.min_tables)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("dump:       {}\n", self.dump_path));
        out.push_str(&format!("format:     {}\n", self.format.label()));
        out.push_str(&format!("scratch db: {}\n", self.scratch_db));
        out.push_str(&format!(
            "restore:    {} via {} ({:.1}s)\n",
            if self.restore.success { "ok" } else { "FAILED" },
            self.restore.program,
            self.elapsed_secs
        ));

        if !self.restore.success {
            let tail = tail_lines(&self.restore.stderr, 20);
            out.push_str("\n--- last output from the restore program ---\n");
            out.push_str(&tail);
            out.push('\n');
            out.push_str("\nFAIL: the dump did not restore cleanly\n");
            return out;
        }

        match &self.contents {
            Some(c) => {
                out.push_str(&format!("tables:     {}\n", c.table_count));
                if let Some(rows) = c.total_rows {
                    out.push_str(&format!("total rows: {rows}\n"));
                }
                if c.table_count < self.min_tables {
                    out.push_str(&format!(
                        "\nFAIL: restore exited 0 but only {} table(s) landed in \
                         the public schema (--min-tables {} required) — the dump \
                         likely restored an empty or near-empty database\n",
                        c.table_count, self.min_tables
                    ));
                } else {
                    out.push_str("\nOK: the dump restores cleanly\n");
                }
            }
            None => out.push_str(
                "\nFAIL: restore program exited 0 but contents were never inspected \
                 (internal error — this should not happen)\n",
            ),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_outcome(success: bool) -> RestoreOutcome {
        RestoreOutcome {
            program: "pg_restore".to_string(),
            success,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn base_report(success: bool, contents: Option<RestoredContents>) -> CheckReport {
        CheckReport {
            dump_path: "backup.dump".to_string(),
            format: DumpFormat::Custom,
            scratch_db: "pgbackupcheck_test".to_string(),
            restore: base_outcome(success),
            contents,
            min_tables: 1,
            elapsed_secs: 1.5,
        }
    }

    #[test]
    fn passes_when_restore_ok_and_tables_meet_minimum() {
        let report = base_report(
            true,
            Some(RestoredContents {
                table_count: 3,
                total_rows: Some(42),
            }),
        );
        assert!(report.passed());
        assert!(report.render().contains("OK: the dump restores cleanly"));
    }

    #[test]
    fn fails_when_the_restore_program_itself_failed() {
        let mut report = base_report(false, None);
        report.restore.stderr = "pg_restore: error: could not read from input file\n".to_string();
        assert!(!report.passed());
        let rendered = report.render();
        assert!(rendered.contains("FAILED"));
        assert!(rendered.contains("could not read from input file"));
        assert!(rendered.contains("FAIL: the dump did not restore cleanly"));
    }

    #[test]
    fn fails_when_restore_succeeds_but_below_min_tables() {
        let report = base_report(
            true,
            Some(RestoredContents {
                table_count: 0,
                total_rows: Some(0),
            }),
        );
        assert!(!report.passed());
        assert!(report.render().contains("only 0 table(s)"));
    }

    #[test]
    fn passes_at_exactly_min_tables_boundary() {
        let mut report = base_report(
            true,
            Some(RestoredContents {
                table_count: 2,
                total_rows: None,
            }),
        );
        report.min_tables = 2;
        assert!(report.passed());
    }

    #[test]
    fn render_omits_row_count_when_not_collected() {
        let report = base_report(
            true,
            Some(RestoredContents {
                table_count: 5,
                total_rows: None,
            }),
        );
        let rendered = report.render();
        assert!(rendered.contains("tables:     5"));
        assert!(!rendered.contains("total rows"));
    }

    #[test]
    fn missing_contents_after_a_successful_restore_never_reports_ok() {
        // Shouldn't happen in practice (inspect always runs right after a
        // successful restore) but the renderer must not silently claim
        // success on a defensive `None` — that would be worse than a panic.
        let report = base_report(true, None);
        assert!(!report.passed());
        assert!(!report.render().contains("OK:"));
    }
}
