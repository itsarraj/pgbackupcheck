# pgbackupcheck

Actually restores a `pg_dump` file into a scratch database and reports
whether it comes back clean — the fix for the classic "we've been taking
backups for two years and never once tried restoring one" failure mode.
`pg_dump` exiting 0 only proves it could *read* the source database; it
says nothing about whether the resulting file will ever successfully
become a database again. This runs the other half of that story for real,
every time, instead of leaving it as an assumption.

## Usage

```bash
pgbackupcheck backup.dump --admin-conn "postgres://postgres@localhost:5432/postgres"
pgbackupcheck backup.sql  --admin-conn "postgres://postgres@localhost:5432/postgres" --count-rows
pgbackupcheck backup.dump --admin-conn "$ADMIN_DSN" --min-tables 12 --keep
```

```
$ pgbackupcheck backup.dump --admin-conn "postgres://postgres@localhost:5432/postgres" --count-rows
created scratch database "pgbackupcheck_1789845253_105199"

dump:       backup.dump
format:     custom
scratch db: pgbackupcheck_1789845253_105199
restore:    ok via pg_restore (0.0s)
tables:     2
total rows: 5

OK: the dump restores cleanly
```

`--admin-conn` needs `CREATE DATABASE` privilege and normally points at a
server's `postgres` maintenance database — it is used only to create and
drop the disposable scratch database, never to touch anything you already
have. The dump is restored into that scratch database, inspected, then
the scratch database is dropped, all in one run. Exit code `0` means the
restore succeeded *and* something real ended up in the schema; `1` means
either the restore program itself failed or it exited 0 into an
implausibly empty database.

## What it actually checks

1. **Detects the dump format** from its own bytes — `pg_dump`'s custom
   format (`-Fc`) starts with the 5-byte magic `PGDMP`; a directory
   format (`-Fd`) is a real directory with a `toc.dat`; a tar archive
   (`-Ft`) has the POSIX `ustar` magic 257 bytes in; anything else is
   treated as plain SQL text (`-Fp`, the default). This matters because
   `pg_restore` flatly refuses plain-text dumps ("please use psql") and
   `psql -f` can't read any of the binary formats — guessing wrong is an
   instant, uninformative failure, not a soft error. `--format` overrides
   detection when a dump has an unusual extension or truncated header.
2. **Creates a real scratch database** (`CREATE DATABASE ... TEMPLATE
   template0`, `LC_COLLATE 'C' LC_CTYPE 'C'` — deliberately never inherits
   a server's possibly-customized `template1`) and refuses to proceed if a
   database by that name already exists, so a re-run never restores over
   leftovers from a crashed previous run.
3. **Restores for real** with the actual `pg_restore` or `psql` binary —
   this tool does not parse or replicate either dump format itself.
4. **Counts what landed** in the `public` schema afterward
   (`information_schema.tables`), and fails the run if it's below
   `--min-tables` (default 1) even when the restore program itself
   exited 0 — catching the case of a "successful" restore of an
   effectively empty database. `--count-rows` additionally sums a
   `count(*)` per table (off by default: it's a real per-table scan, slow
   on a large dump).
5. **Always drops the scratch database afterward**, including when the
   restore failed, unless `--keep` is passed to leave it for inspection.

## Status: built and live-verified end to end, including both the success and the deliberately-broken path

- **35 unit tests** (`cargo test --lib`), all pure logic, no database
  needed: dump-format sniffing from raw bytes (`PGDMP` magic, the `ustar`
  magic sitting 257 bytes in rather than at offset 0, a directory with and
  without `toc.dat`, buffers shorter than the magic without panicking);
  connection-string `dbname` substitution for both the URI form
  (`postgres://user:pw@host:5432/olddb?sslmode=require`, checked that the
  password, port, and query string all survive) and the libpq keyword
  form, including appending `dbname=` when it was absent from either
  shape; database-identifier validation rejecting anything that isn't
  `[A-Za-z_][A-Za-z0-9_]*` up to 63 characters (including a literal
  `"; DROP TABLE users; --` attempt); and the report renderer's pass/fail
  logic at the `--min-tables` boundary and for a restore that fails
  outright.
- **The real success path, run against a real Postgres 18.6**: created a
  scratch source database with two tables (`widgets`, `orders`, a foreign
  key between them, 5 rows total), ran real `pg_dump -Fc` and `pg_dump
  -Fp` against it, then ran `pgbackupcheck` against *both* resulting
  files with `--count-rows`. Both correctly auto-detected their format
  (`custom` via `pg_restore`, `plain (SQL text)` via `psql`), both
  restored cleanly, and both reported back the true numbers — **2 tables,
  5 total rows** — matching the source exactly. Exit code was `0` in both
  cases, and the scratch database (a fresh, different name each run) was
  gone afterward, confirmed via `psql -l`.
- **The real failure path, on both formats**: truncated the custom-format
  dump to 3000 of its 5021 bytes and the plain-SQL dump to 2000 of its
  3578 (mid-`ALTER TABLE` statement) and ran `pgbackupcheck` against each.
  The custom-format one failed with `pg_restore: error: could not read
  from input file: end of file`; the plain one failed with `psql`'s own
  `ERROR: syntax error at end of input` pointing at the truncated line.
  Both were reported as `restore: FAILED`, exit code `1`, and — this is
  the part that actually matters for a tool meant to run unattended in a
  backup-verification cron job — **the scratch database was still
  cleanly dropped in both failure cases**, confirmed via `psql -l`
  immediately after (only the database I had created by hand for the
  test was left; no `pgbackupcheck_*` leftovers).
- **The "successful but empty" trap, also verified live**: dumped a
  genuinely empty database and ran `pgbackupcheck` against it.
  `pg_restore` itself exited `0` (there was nothing in the dump to fail
  on) but `pgbackupcheck` still reported `FAIL: restore exited 0 but only
  0 table(s) landed in the public schema` and exited `1` — this is
  exactly the gap a bare `pg_restore && echo ok` check in a shell script
  would miss.
- **A real footgun found and worked around along the way**: the first
  version of `create_scratch_db` used a plain `CREATE DATABASE "name"`,
  which on this machine failed outright with `ERROR: template database
  "template1" has a collation version mismatch` — a stale collation
  provider version left over from an OS library upgrade, a real and
  apparently common failure mode on long-lived Postgres hosts. Restoring
  through `TEMPLATE template0` with an explicit `LC_COLLATE 'C'`/
  `LC_CTYPE 'C'` sidesteps it entirely, and is arguably the more correct
  choice regardless: a disposable scratch database has no reason to
  inherit whatever locale or extensions someone accumulated on this
  server's `template1` over the years.

**Not done / deliberately deferred**:
- **No timeout on the restore subprocess.** A dump that makes
  `pg_restore`/`psql` hang (a network filesystem stall, a lock wait
  against a busy scratch server) will hang `pgbackupcheck` too — there is
  no `--timeout` yet.
- **No parallel restore.** `pg_restore -j` isn't wired up; every restore
  runs single-threaded, which is correct for a verification tool (you
  want to know it restores, not how fast) but slower than it needs to be
  on a large custom-format dump.
- **No schema/data-only distinction.** A `--schema-only` or `--data-only`
  dump restores "successfully" and reports its (possibly zero, possibly
  nonsensical) table/row counts the same as a full dump — the tool has no
  way to know which kind of dump it was handed and adjust expectations.
- **No content verification beyond table count and optional row count.**
  This does not diff the restored schema against the source, checksum any
  column, or verify foreign keys/constraints beyond whatever the restore
  program itself enforces while loading. It answers "did this file turn
  back into a database," not "is this byte-for-byte the same database."
- **`--admin-conn` must have `CREATE DATABASE` privilege on the target
  server.** There's no fallback path (e.g., delegating to an existing
  database someone already provisioned) for a locked-down server that
  doesn't grant that.
