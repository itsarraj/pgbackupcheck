use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use pgbackupcheck::{conninfo, db, format, report::CheckReport, runner};

#[derive(Parser)]
#[command(
    name = "pgbackupcheck",
    version,
    about = "Restores a pg_dump file into a scratch database for real, and reports whether it actually comes back clean"
)]
struct Cli {
    /// The pg_dump output to check — a plain SQL file, a `-Fc` custom
    /// archive, a `-Ft` tar archive, or a `-Fd` directory.
    dump: PathBuf,

    /// A connection string with CREATE DATABASE privilege — used only to
    /// create and drop the scratch database. Typically points at the
    /// `postgres` maintenance database. Accepts either
    /// `postgres://user@host:5432/postgres` or libpq keyword form.
    #[arg(long)]
    admin_conn: String,

    /// Name for the scratch database. Auto-generated (and guaranteed not
    /// to already exist) when omitted.
    #[arg(long)]
    scratch_db: Option<String>,

    /// Don't drop the scratch database afterward — useful for inspecting a
    /// failed restore by hand.
    #[arg(long)]
    keep: bool,

    /// Force the dump format instead of auto-detecting it from content.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,

    /// Fail if fewer than this many tables land in the public schema, even
    /// if the restore program itself exits 0. Catches an "empty but
    /// technically successful" restore.
    #[arg(long, default_value_t = 1)]
    min_tables: i64,

    /// Also sum row counts across every restored table (one `count(*)` per
    /// table — skipped by default since it can be slow on a large dump).
    #[arg(long)]
    count_rows: bool,

    /// Path to the `pg_restore` binary.
    #[arg(long, default_value = "pg_restore")]
    pg_restore_bin: String,

    /// Path to the `psql` binary.
    #[arg(long, default_value = "psql")]
    psql_bin: String,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum FormatArg {
    Plain,
    Custom,
    Tar,
    Directory,
}

impl From<FormatArg> for format::DumpFormat {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Plain => format::DumpFormat::Plain,
            FormatArg::Custom => format::DumpFormat::Custom,
            FormatArg::Tar => format::DumpFormat::Tar,
            FormatArg::Directory => format::DumpFormat::Directory,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.dump.exists() {
        anyhow::bail!("{} does not exist", cli.dump.display());
    }

    let fmt = match cli.format {
        Some(f) => f.into(),
        None => format::detect(&cli.dump)
            .with_context(|| format!("detecting the format of {}", cli.dump.display()))?,
    };

    let scratch_db = cli.scratch_db.clone().unwrap_or_else(|| {
        format!(
            "pgbackupcheck_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            std::process::id()
        )
    });

    let mut admin = db::connect(&cli.admin_conn)?;
    db::create_scratch_db(&mut admin, &scratch_db)?;
    println!("created scratch database \"{scratch_db}\"");

    // Everything from here on must still reach the cleanup step even on
    // error, so failures are captured rather than propagated with `?`.
    let restore_conn = conninfo::with_dbname(&cli.admin_conn, &scratch_db);
    let result = restore_conn.and_then(|restore_conn| {
        let start = Instant::now();
        let outcome = runner::restore(
            fmt,
            &cli.dump,
            &restore_conn,
            &cli.pg_restore_bin,
            &cli.psql_bin,
        )?;
        let elapsed = start.elapsed().as_secs_f64();

        let contents = if outcome.success {
            let mut target = db::connect(&restore_conn)?;
            Some(db::inspect_restored_db(&mut target, cli.count_rows)?)
        } else {
            None
        };

        Ok(CheckReport {
            dump_path: cli.dump.display().to_string(),
            format: fmt,
            scratch_db: scratch_db.clone(),
            restore: outcome,
            contents,
            min_tables: cli.min_tables,
            elapsed_secs: elapsed,
        })
    });

    if !cli.keep {
        if let Err(e) = db::drop_scratch_db(&mut admin, &scratch_db) {
            eprintln!("pgbackupcheck: warning: failed to drop scratch database: {e:#}");
        }
    } else {
        println!("--keep set: leaving \"{scratch_db}\" in place");
    }

    let report = result?;
    print!("\n{}", report.render());

    if report.passed() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
