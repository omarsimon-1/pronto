//! First-run copies from a Dispatchy install, the name this app had before Pronto. Originals and existing Pronto files
//! always win: publish complete copies atomically, without replacing a destination. Each file
//! is decided by itself, so a Pronto directory that already exists (a debug build ran first, or
//! an earlier pass stopped half way) still receives what it lacks.

use crate::store::{StoreError, StoreLock};
use crate::xdg::{SealwaxDirs, ensure_private_dir};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LEGACY_DIR: &str = "dispatchy";
const COMPLETED: &str = ".dispatchy-migration-v1";
const STAGING_PREFIX: &str = ".pronto-migrate-";
/// Where a Pronto store that held nothing goes when Dispatchy's takes its place.
const SET_ASIDE: &str = ".before-dispatchy";

/// Whether a first migration pass is needed. The launcher checks this before checking that the
/// legacy app has stopped, so a later launch does not depend on the old app's state.
pub fn needed(dirs: &SealwaxDirs) -> io::Result<bool> {
    if exists(&dirs.config.join(COMPLETED))? {
        return Ok(false);
    }
    for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
        if exists(&dest.with_file_name(LEGACY_DIR))? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn from_dispatchy(dirs: &SealwaxDirs) -> io::Result<()> {
    if !needed(dirs)? {
        return Ok(());
    }
    match fs::symlink_metadata(&dirs.config) {
        Ok(meta) if meta.is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Pronto's configuration directory is a symlink; use a separate directory before migrating",
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
        remove_stale_staging(dest)?;
    }
    // A Pronto store may already exist with nothing in it, created by a build that does not
    // migrate. It must not keep Dispatchy's mail out, so it steps aside before the copy.
    let legacy_store = dirs.data.with_file_name(LEGACY_DIR).join(STORE_FILE);
    if exists(&legacy_store)? {
        make_way_for_store(&dirs.store_file())?;
    }
    for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
        merge_missing(&dest.with_file_name(LEGACY_DIR), dest)?;
    }
    // Dispatchy kept writing-style sheets beside its data directory, in `dispatch/style`.
    if let Some(parent) = dirs.data.parent() {
        merge_missing(&parent.join("dispatch/style"), &dirs.data.join("style"))?;
    }
    // The copies must be on disk before the marker says so: after a power cut a marker beside
    // half-written copies would end the retries.
    for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
        if exists(dest)? {
            sync_filesystem(dest)?;
        }
    }
    // Mark only a completed pass, so later intentional deletions are not restored.
    crate::settings_files::create_if_missing(&dirs.config.join(COMPLETED), "complete\n")?;
    Ok(())
}

const STORE_FILE: &str = "store.sqlite";

/// SQLite's files beside the store. The backup includes committed WAL transactions, and a WAL
/// or SHM of another database beside that snapshot could corrupt it.
fn is_store_journal(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("store.sqlite-wal" | "store.sqlite-shm" | "store.sqlite-journal")
    )
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Staging directories an interrupted pass left beside `dest`: whole copies nothing refers to.
fn remove_stale_staging(dest: &Path) -> io::Result<()> {
    let Some(parent) = dest.parent() else {
        return Ok(());
    };
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Leaves no Pronto store at `store` when the one there holds no account, message or draft, so
/// Dispatchy's is copied in its place; a store that holds any stays and wins. The empty store
/// is renamed, never deleted. Safe to run again at any point: with the store renamed and the
/// copy not yet published, the next pass finds no store and copies.
fn make_way_for_store(store: &Path) -> io::Result<()> {
    let wal = with_suffix(store, "-wal");
    let shm = with_suffix(store, "-shm");
    if !exists(store)? {
        // Left by a pass that stopped after renaming the store: they belong to no database.
        for stale in [&wal, &shm] {
            remove_if_present(stale)?;
        }
        return Ok(());
    }
    // A running Pronto holds this lock; replacing the store under it would lose its writes.
    let _lock = StoreLock::acquire(store).map_err(|error| match error {
        StoreError::Locked(_) => io::Error::other(
            "quit every running Pronto (also debug builds) before the first launch, then retry",
        ),
        StoreError::Io(error) => error,
        other => io::Error::other(other),
    })?;
    let empty = (|| -> rusqlite::Result<bool> {
        let conn = Connection::open(store)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        for table in ["accounts", "messages", "drafts"] {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )?;
            if present
                && conn.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
                    [],
                    |row| row.get(0),
                )?
            {
                return Ok(false);
            }
        }
        // Everything into the database file, so the WAL beside it holds nothing to lose.
        let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("Pronto's mail store is in use; quit Pronto and retry".into()),
            ));
        }
        Ok(true)
    })()
    .map_err(io::Error::other)?;
    if !empty {
        return Ok(());
    }
    // The WAL and SHM go first: beside Dispatchy's copy they would be another database's.
    for stale in [&wal, &shm] {
        remove_if_present(stale)?;
    }
    let mut aside = with_suffix(store, SET_ASIDE);
    let mut n = 1;
    while exists(&aside)? {
        n += 1;
        aside = with_suffix(store, &format!("{SET_ASIDE}-{n}"));
    }
    rename_noreplace(store, &aside)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Copies what `dest` lacks from `source`: a missing file or directory whole, and an existing
/// directory entry by entry. A file Pronto already has is never replaced or written through.
fn merge_missing(source: &Path, dest: &Path) -> io::Result<()> {
    let source_meta = match fs::symlink_metadata(source) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let dest_meta = match fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return copy_missing(source, dest),
        Err(e) => return Err(e),
    };
    if source_meta.is_dir() && dest_meta.is_dir() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if is_store_journal(&entry.file_name()) {
                continue;
            }
            merge_missing(&entry.path(), &dest.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Flushes the file system holding `dir`: the copies and the renames that published them.
fn sync_filesystem(dir: &Path) -> io::Result<()> {
    let dir = fs::File::open(dir)?;
    // SAFETY: the descriptor stays open for the call.
    if unsafe { libc::syncfs(dir.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn copy_missing(source: &Path, dest: &Path) -> io::Result<()> {
    if exists(dest)? || !exists(source)? {
        return Ok(());
    }
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::other("no destination parent"))?;
    if !parent.exists() {
        ensure_private_dir(parent)?;
    }
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent)?;
    let ready = staging.path().join("ready");
    copy_tree(source, &ready)?;
    publish(&ready, dest)
}

fn copy_tree(source: &Path, dest: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() {
        ensure_private_dir(dest)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if is_store_journal(&entry.file_name()) {
                continue;
            }
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        if source.file_name().is_some_and(|name| name == STORE_FILE) {
            copy_database(source, dest)?;
        } else {
            fs::copy(source, dest)?;
        }
        fs::set_permissions(
            dest,
            fs::Permissions::from_mode(meta.permissions().mode() & 0o700),
        )?;
    } else {
        // Following a link could make later edits affect the original files. Leave the
        // migration unpublished and ask the owner to resolve it, rather than sharing data.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "legacy data contains a symlink or special file; copy it to a regular file before migrating",
        ));
    }
    Ok(())
}

fn copy_database(source: &Path, dest: &Path) -> io::Result<()> {
    let result = (|| -> rusqlite::Result<()> {
        let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut dest = Connection::open(dest)?;
        let backup = Backup::new(&source, &mut dest)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if backup.step(256)? == StepResult::Done {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("Dispatchy's mail store stayed busy; quit Dispatchy and retry".into()),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    result.map_err(io::Error::other)
}

/// Publishes a staged copy; a destination another process created meanwhile wins.
fn publish(source: &Path, dest: &Path) -> io::Result<()> {
    match rename_noreplace(source, dest) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

fn rename_noreplace(source: &Path, dest: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())?;
    let dest = CString::new(dest.as_os_str().as_bytes())?;
    // SAFETY: both paths are valid NUL-terminated strings. Linux's RENAME_NOREPLACE
    // atomically publishes the copy only if no other process created the destination.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::blobs::BlobStore;
    use crate::store::{NewAccount, Store, StoreOptions};

    /// A store with the real schema at `path`, holding the named accounts.
    fn store_with(path: &Path, accounts: &[&str]) {
        let store = Store::open(path, StoreOptions::default()).unwrap();
        for name in accounts {
            store
                .add_account(NewAccount {
                    name: (*name).into(),
                    address: format!("{name}@example.test"),
                })
                .recv_blocking()
                .unwrap();
        }
        store.close().unwrap();
    }

    fn account_names(path: &Path) -> Vec<String> {
        let db = Connection::open(path).unwrap();
        let mut statement = db.prepare("SELECT name FROM accounts ORDER BY id").unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// Dispatchy's data: a store with one account, two blobs and a style sheet.
    fn legacy_data(dirs: &SealwaxDirs) -> (PathBuf, Vec<sealwax_domain::model::BlobKey>) {
        let old = dirs.data.with_file_name(LEGACY_DIR);
        store_with(&old.join(STORE_FILE), &["fictional"]);
        let blobs = BlobStore::open(&old.join("blobs")).unwrap();
        let keys = vec![
            blobs.put(b"Subject: one\r\n\r\n").unwrap(),
            blobs.put(b"Subject: two\r\n\r\n").unwrap(),
        ];
        let style = dirs.data.with_file_name("dispatch").join("style");
        fs::create_dir_all(&style).unwrap();
        fs::write(style.join("voice.md"), "fictional voice").unwrap();
        (old, keys)
    }

    #[test]
    fn an_empty_pronto_store_steps_aside_for_dispatchys() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let (_, keys) = legacy_data(&dirs);
        // A build that does not migrate ran first: a store with no account, a blob store, and
        // files SQLite left beside the store.
        store_with(&dirs.store_file(), &[]);
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        fs::write(dirs.data.join("style"), "not a directory").unwrap();
        assert!(needed(&dirs).unwrap());
        from_dispatchy(&dirs).unwrap();
        assert!(!needed(&dirs).unwrap());
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
        assert!(account_names(&with_suffix(&dirs.store_file(), SET_ASIDE)).is_empty());
        for key in &keys {
            assert!(blobs.contains(key));
            assert_eq!(
                fs::metadata(blobs.path(key)).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // A file Pronto has is kept, also where Dispatchy has a directory.
        assert_eq!(
            fs::read_to_string(dirs.data.join("style")).unwrap(),
            "not a directory"
        );
        // The migrated store opens, and nothing is left in the staging areas.
        store_with(&dirs.store_file(), &[]);
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
        for dir in [dirs.data.parent().unwrap(), dirs.data.as_path()] {
            for entry in fs::read_dir(dir).unwrap() {
                let name = entry.unwrap().file_name();
                assert!(!name.to_str().unwrap().starts_with(STAGING_PREFIX));
            }
        }
    }

    #[test]
    fn a_pronto_store_with_mail_wins_and_still_gains_missing_files() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let (old, keys) = legacy_data(&dirs);
        store_with(&dirs.store_file(), &["pronto"]);
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        // The same content under the same name, already Pronto's: left alone.
        assert_eq!(blobs.put(b"Subject: one\r\n\r\n").unwrap(), keys[0]);
        let before = fs::metadata(blobs.path(&keys[0]))
            .unwrap()
            .modified()
            .unwrap();
        from_dispatchy(&dirs).unwrap();
        assert_eq!(account_names(&dirs.store_file()), ["pronto"]);
        assert!(!with_suffix(&dirs.store_file(), SET_ASIDE).exists());
        assert!(blobs.contains(&keys[1]));
        assert_eq!(
            fs::metadata(blobs.path(&keys[0]))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        assert_eq!(
            fs::read_to_string(dirs.data.join("style/voice.md")).unwrap(),
            "fictional voice"
        );
        assert_eq!(account_names(&old.join(STORE_FILE)), ["fictional"]);
    }

    #[test]
    fn an_interrupted_pass_finishes_on_the_next_and_then_stays_done() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let (_, keys) = legacy_data(&dirs);
        store_with(&dirs.store_file(), &[]);
        // The pass stops in the state directory, after the data directory was merged.
        let state = dirs.state.with_file_name(LEGACY_DIR);
        fs::create_dir_all(&state).unwrap();
        let link = state.join("link");
        std::os::unix::fs::symlink("missing", &link).unwrap();
        BlobStore::open(&dirs.blobs_dir()).unwrap();
        // What an interrupted earlier pass left behind.
        let staging = dirs.data.with_file_name(format!("{STAGING_PREFIX}left"));
        fs::create_dir_all(staging.join("ready")).unwrap();
        assert!(from_dispatchy(&dirs).is_err());
        assert!(!staging.exists());
        assert!(needed(&dirs).unwrap());
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
        fs::remove_file(&link).unwrap();
        from_dispatchy(&dirs).unwrap();
        assert!(!needed(&dirs).unwrap());
        // The store copied by the first pass was not set aside or copied again.
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
        assert!(!with_suffix(&dirs.store_file(), &format!("{SET_ASIDE}-2")).exists());
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        assert!(keys.iter().all(|key| blobs.contains(key)));
        // Done: what the user deletes later stays deleted.
        blobs.remove(&keys[0]).unwrap();
        from_dispatchy(&dirs).unwrap();
        assert!(!blobs.contains(&keys[0]));
    }

    #[test]
    fn a_store_renamed_by_an_interrupted_pass_leaves_no_stale_wal() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        legacy_data(&dirs);
        ensure_private_dir(&dirs.data).unwrap();
        fs::write(
            with_suffix(&dirs.store_file(), "-wal"),
            "another database's",
        )
        .unwrap();
        fs::write(
            with_suffix(&dirs.store_file(), "-shm"),
            "another database's",
        )
        .unwrap();
        from_dispatchy(&dirs).unwrap();
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
    }

    #[test]
    fn a_running_pronto_keeps_its_store() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        legacy_data(&dirs);
        let open = Store::open(&dirs.store_file(), StoreOptions::default()).unwrap();
        let error = from_dispatchy(&dirs).unwrap_err();
        assert!(error.to_string().contains("quit every running Pronto"));
        assert!(needed(&dirs).unwrap());
        assert!(!with_suffix(&dirs.store_file(), SET_ASIDE).exists());
        open.close().unwrap();
        from_dispatchy(&dirs).unwrap();
        assert_eq!(account_names(&dirs.store_file()), ["fictional"]);
    }

    #[test]
    fn migration_copies_all_roots_without_changing_old_or_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
            let source = dest.with_file_name(LEGACY_DIR);
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("fixture"), "original").unwrap();
        }
        let legacy = dirs.config.with_file_name(LEGACY_DIR);
        fs::write(legacy.join("accounts.toml"), "# fictional accounts").unwrap();
        ensure_private_dir(&dirs.config).unwrap();
        fs::write(dirs.config.join("existing"), "preserved").unwrap();
        assert!(needed(&dirs).unwrap());
        from_dispatchy(&dirs).unwrap();
        assert!(!needed(&dirs).unwrap());
        assert_eq!(
            fs::read(dirs.accounts_file()).unwrap(),
            fs::read(legacy.join("accounts.toml")).unwrap()
        );
        assert_eq!(
            fs::read_to_string(dirs.config.join("existing")).unwrap(),
            "preserved"
        );
        for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
            assert_eq!(
                fs::read_to_string(dest.join("fixture")).unwrap(),
                "original"
            );
            assert_eq!(
                fs::metadata(dest).unwrap().permissions().mode() & 0o777,
                0o700
            );
            fs::write(dest.join("fixture"), "new").unwrap();
        }
        fs::remove_file(dirs.accounts_file()).unwrap();
        assert!(!needed(&dirs).unwrap());
        from_dispatchy(&dirs).unwrap();
        assert!(!dirs.accounts_file().exists());
        assert_eq!(
            fs::read_to_string(legacy.join("accounts.toml")).unwrap(),
            "# fictional accounts"
        );
        for dest in [&dirs.config, &dirs.data, &dirs.state, &dirs.cache] {
            assert_eq!(fs::read_to_string(dest.join("fixture")).unwrap(), "new");
            assert_eq!(
                fs::read_to_string(dest.with_file_name(LEGACY_DIR).join("fixture")).unwrap(),
                "original"
            );
        }
    }

    #[test]
    fn migration_snapshots_committed_wal_data() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let old = dirs.data.with_file_name(LEGACY_DIR);
        fs::create_dir_all(&old).unwrap();
        let db = Connection::open(old.join("store.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE fixture (value TEXT); INSERT INTO fixture VALUES ('fictional');").unwrap();
        assert!(old.join("store.sqlite-wal").exists());
        from_dispatchy(&dirs).unwrap();
        let copied = Connection::open(dirs.store_file()).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT value FROM fixture", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "fictional"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn migration_failure_does_not_publish_a_partial_directory() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let old = dirs.data.with_file_name(LEGACY_DIR);
        fs::create_dir_all(&old).unwrap();
        std::os::unix::fs::symlink("missing", old.join("link")).unwrap();
        assert!(from_dispatchy(&dirs).is_err());
        assert!(!dirs.data.exists());
        assert!(!dirs.config.join(COMPLETED).exists());
        assert!(fs::symlink_metadata(old.join("link")).unwrap().is_symlink());
    }

    #[test]
    fn migration_never_writes_through_a_config_symlink() {
        let root = tempfile::tempdir().unwrap();
        let dirs = SealwaxDirs::under(root.path());
        let old = dirs.config.with_file_name(LEGACY_DIR);
        fs::create_dir_all(&old).unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(old.join("config.toml"), "# fictional settings").unwrap();
        std::os::unix::fs::symlink(&old, &dirs.config).unwrap();
        assert_eq!(
            from_dispatchy(&dirs).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!old.join(COMPLETED).exists());
        assert_eq!(
            fs::metadata(&old).unwrap().permissions().mode() & 0o777,
            0o750
        );
        assert_eq!(
            fs::read_to_string(old.join("config.toml")).unwrap(),
            "# fictional settings"
        );
    }
}
