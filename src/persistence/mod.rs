//! Plan files in the data directory (SPEC 15).
//!
//! Each plan is kept in a SQLite file of its own. A connection is opened for
//! each save and closed after it, so nothing holds a file open between
//! requests, a file can be renamed or deleted at any time, and each save sees
//! the file as it is on disk. Plan files use SQLite's rollback journal, which
//! is deleted when a transaction ends, so none is left beside a file between
//! requests (SPEC 15.6). A file that comes with journal files beside it, or in
//! WAL mode, is put right when the server starts ([`prepare`]).

mod names;
mod records;

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use rusqlite_migration::{M, Migrations};

use crate::model::{Plan, PlanInfo, new_id};
use names::{file_names, safe_name};
use records::{read_plan, write_plan};

/// Every schema version's migration: version `n` is the state after the
/// first `n` (SPEC 15.8). A shipped migration never changes; each change to
/// what a plan file holds adds one.
const MIGRATIONS: &[M] =
    &[M::up(include_str!("../../migrations/01-plans/up.sql")).down(include_str!("../../migrations/01-plans/down.sql"))];

/// The schema version this server writes.
const LATEST: usize = MIGRATIONS.len();

fn migrations() -> Migrations<'static> {
    Migrations::from_slice(MIGRATIONS)
}

/// Why a file couldn't be used, or a change couldn't be saved. Only shown on
/// stderr.
pub type Failure = Box<dyn Error + Send + Sync>;

/// Opens an existing file, never creating one.
fn connect(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
}

/// The schema version of a plan file the server knows, from 1 to [`LATEST`]
/// (SPEC 15.8).
fn schema_version(conn: &Connection) -> Result<usize, Failure> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match usize::try_from(version) {
        Ok(v @ 1..=LATEST) => Ok(v),
        Ok(v) if v > LATEST => Err(format!("its schema version, {v}, is newer than this server's, {LATEST}").into()),
        _ => Err(format!("it isn't a plan file: its schema version is {version}").into()),
    }
}

/// Reads the plan in a plan file, without changing the file. A file at an
/// older version is read from a copy upgraded in memory, so that a file that
/// can't be upgraded, or holds data the server can't read once it is, is left
/// as it was (SPEC 15.7, 15.8).
fn read_plan_file(path: &Path) -> Result<Plan, Failure> {
    let conn = connect(path)?;
    if schema_version(&conn)? == LATEST {
        return read_plan(&conn);
    }
    let mut copy = Connection::open_in_memory()?;
    Backup::new(&conn, &mut copy)?.run_to_completion(i32::MAX, Duration::ZERO, None)?;
    migrations().to_latest(&mut copy)?;
    read_plan(&copy)
}

/// Upgrades a file to the latest schema version, in one transaction
/// (SPEC 15.8).
fn upgrade(path: &Path) -> Result<(), Failure> {
    Ok(migrations().to_latest(&mut connect(path)?)?)
}

/// Readies a file the server has read and will serve: upgrades it
/// (SPEC 15.8), then leaves it a complete database on its own, with no
/// journal files beside it (SPEC 15.6).
///
/// Leaving WAL mode moves what the WAL holds into the file and deletes the
/// WAL and its index. Any journal file still there is one SQLite ignores: a
/// rollback journal that holds no transaction, as `journal_mode = PERSIST`
/// or `TRUNCATE` leave behind, an empty WAL, or a WAL index without a WAL.
/// SQLite rolled back any journal that did hold one when the file was read,
/// so what's left holds nothing the file needs, and is deleted.
fn prepare(path: &Path) -> Result<(), Failure> {
    upgrade(path)?;
    let mode: String = connect(path)?.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(format!("it can't leave journal mode {mode}").into());
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut journal = path.as_os_str().to_owned();
        journal.push(suffix);
        if let Err(e) = fs::remove_file(&journal)
            && e.kind() != io::ErrorKind::NotFound
        {
            return Err(e.into());
        }
    }
    Ok(())
}

/// Writes the changes from `old` to `new` in one SQLite transaction, so the
/// file gets all of them or none (SPEC 15.10).
fn write_file(path: &Path, old: Option<&Plan>, new: &Plan) -> Result<(), Failure> {
    let mut conn = connect(path)?;
    let tx = conn.transaction()?;
    write_plan(&tx, old, new)?;
    Ok(tx.commit()?)
}

/// Whether anything in the directory has the name of `path`, or one the file
/// system treats as the same, other than the file `own` (SPEC 15.2, 15.4).
fn is_taken(path: &Path, own: Option<&Path>) -> bool {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(_) => !own.is_some_and(|own| same_file::is_same_file(path, own).unwrap_or(false)),
    }
}

/// Writes a line to stderr naming a file the server won't serve, and why
/// (SPEC 15.7).
fn skip(path: &Path, reason: impl std::fmt::Display) {
    eprintln!("skipping {}: {reason}", path.display());
}

/// Checks the server can create, write and delete files in `dir` (SPEC 15.1).
fn check_writable(dir: &Path) -> io::Result<()> {
    let probe = dir.join(format!(".mybudget-write-check-{}", new_id()));
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)
}

/// The data directory and the file each plan in it is saved in.
pub struct PlanFiles {
    dir: PathBuf,
    /// By plan id.
    paths: HashMap<String, PathBuf>,
}

impl PlanFiles {
    /// Opens the data directory, creating it if it doesn't exist (SPEC 15.1),
    /// and reads the plans in it, oldest first (SPEC 15.5). Files the server
    /// can't use are skipped (SPEC 15.7).
    pub fn open(dir: &Path) -> Result<(Self, Vec<Plan>), String> {
        let unusable = |e: io::Error| format!("can't use the data directory {}: {e}", dir.display());
        fs::create_dir_all(dir).map_err(unusable)?;
        check_writable(dir).map_err(unusable)?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(dir).map_err(unusable)? {
            let path = entry.map_err(unusable)?.path();
            if path.as_os_str().as_encoded_bytes().ends_with(b".sqlite") {
                paths.push(path);
            }
        }
        paths.sort();

        let mut read = Vec::new();
        for path in paths {
            match read_plan_file(&path) {
                Ok(plan) => read.push((path, plan)),
                Err(reason) => skip(&path, reason),
            }
        }

        let mut files_per_plan: HashMap<String, usize> = HashMap::new();
        for (_, plan) in &read {
            *files_per_plan.entry(plan.info.id.clone()).or_default() += 1;
        }
        let mut paths = HashMap::new();
        let mut plans = Vec::new();
        for (path, plan) in read {
            if files_per_plan[&plan.info.id] > 1 {
                skip(&path, format_args!("another file also holds plan {}", plan.info.id));
                continue;
            }
            // The first change to the file, made once it's known to be
            // served (SPEC 15.7, 15.8).
            match prepare(&path) {
                Ok(()) => {
                    paths.insert(plan.info.id.clone(), path);
                    plans.push(plan);
                }
                Err(reason) => skip(&path, reason),
            }
        }
        plans.sort_by(|a, b| (a.created_at, &a.info.id).cmp(&(b.created_at, &b.info.id)));
        Ok((Self { dir: dir.to_owned(), paths }, plans))
    }

    /// The path a plan's file gets, never one that something in the
    /// directory has, except the plan's own file `own` (SPEC 15.2, 15.4).
    fn free_path(&self, info: &PlanInfo, own: Option<&Path>) -> Result<PathBuf, Failure> {
        file_names(&info.name, &info.id)
            .into_iter()
            .map(|name| self.dir.join(name))
            .find(|path| !is_taken(path, own))
            .ok_or_else(|| "every name the plan's file could have is taken".into())
    }

    /// Saves a new plan in a file of its own (SPEC 15.2).
    pub fn create(&mut self, plan: &Plan) -> Result<(), Failure> {
        let path = self.free_path(&plan.info, None)?;
        // Fails rather than overwriting a file that has appeared since.
        File::create_new(&path)?;
        if let Err(e) = upgrade(&path).and_then(|()| write_file(&path, None, plan)) {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        self.paths.insert(plan.info.id.clone(), path);
        Ok(())
    }

    /// Saves the changes from `old`, the plan as its file holds it, to `new`.
    /// A file whose plan is renamed is renamed to match, unless its safe name
    /// stays the same (SPEC 15.4); if the changes can't be written, it gets
    /// its old name back.
    pub fn save(&mut self, old: &Plan, new: &Plan) -> Result<(), Failure> {
        let path = &self.paths[&new.info.id];
        if safe_name(&old.info.name) == safe_name(&new.info.name) {
            return write_file(path, Some(old), new);
        }
        let renamed = self.free_path(&new.info, Some(path))?;
        fs::rename(path, &renamed)?;
        if let Err(e) = write_file(&renamed, Some(old), new) {
            if let Err(undo) = fs::rename(&renamed, path) {
                eprintln!("couldn't rename {} back to {}: {undo}", renamed.display(), path.display());
            }
            return Err(e);
        }
        self.paths.insert(new.info.id.clone(), renamed);
        Ok(())
    }

    /// Deletes a plan's file (SPEC 15.4).
    pub fn delete(&mut self, plan_id: &str) -> Result<(), Failure> {
        fs::remove_file(&self.paths[plan_id])?;
        self.paths.remove(plan_id);
        Ok(())
    }
}

/// `mybudget-server migrate <file> <version>`: changes a plan file to that
/// schema version, older or newer (SPEC 15.9).
pub fn migrate(file: &Path, version: &str) -> Result<(), String> {
    let target = version
        .parse()
        .ok()
        .filter(|v| (1..=LATEST).contains(v))
        .ok_or_else(|| format!("the version must be an integer from 1 to {LATEST}"))?;
    migrate_file(file, target).map_err(|e| format!("can't migrate {}: {e}", file.display()))
}

/// Changes a file the server can read to `target`, in one transaction; any
/// other file is left as it was.
fn migrate_file(file: &Path, target: usize) -> Result<(), Failure> {
    read_plan_file(file)?;
    Ok(migrations().to_version(&mut connect(file)?, target)?)
}
