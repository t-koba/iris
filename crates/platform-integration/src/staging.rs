//! Persisted local transfer staging. The journal records the path and binding before
//! creation. A held OS lock excludes live owners; dropping it permits restart, not deletion.
use crate::copy::handle_identity;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// SQLite text locator preserving native path bytes (or Windows UTF-16 units).
pub fn encode_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        serde_json::to_string(path.as_os_str().as_bytes()).expect("path bytes")
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        serde_json::to_string(&path.as_os_str().encode_wide().collect::<Vec<_>>())
            .expect("path units")
    }
}

pub fn decode_path(locator: &str) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bytes: Vec<u8> = serde_json::from_str(locator)?;
        Ok(std::ffi::OsString::from_vec(bytes).into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = serde_json::from_str(locator)?;
        Ok(std::ffi::OsString::from_wide(&units).into())
    }
}

#[derive(Serialize, Deserialize)]
struct Owner {
    binding: String,
    len: u64,
}

pub struct OwnedStage {
    dir: PathBuf,
    _owner: File,
    pub data: File,
}

fn invalid() -> io::Error {
    io::Error::other("staging ownership or identity mismatch")
}

fn regular(path: &Path) -> io::Result<()> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid());
    }
    Ok(())
}

struct CreationCleanup(Option<(PathBuf, String, u64)>);
impl Drop for CreationCleanup {
    fn drop(&mut self) {
        if let Some((dir, binding, len)) = &self.0 {
            // Reacquire the lock: a successful concurrent reopen must never lose
            // its data even when the creator encountered a late sync failure.
            let _ = remove(dir, binding, *len);
        }
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn identity(dir: &Path) -> io::Result<(u64, u64)> {
    regular(&dir.join("identity"))?;
    Ok(serde_json::from_reader(File::open(dir.join("identity"))?)?)
}

impl OwnedStage {
    pub fn create(dir: &Path, binding: &str, len: u64) -> io::Result<Self> {
        Self::open_inner(dir, binding, len, true)
    }

    fn open_inner(dir: &Path, binding: &str, len: u64, create: bool) -> io::Result<Self> {
        if create {
            std::fs::create_dir_all(dir.parent().ok_or_else(invalid)?)?;
        }
        let fresh = if !create {
            false
        } else {
            let create_result = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new().mode(0o700).create(dir)
                }
                #[cfg(not(unix))]
                {
                    std::fs::create_dir(dir)
                }
            };
            create_result?;
            true
        };
        if !std::fs::symlink_metadata(dir)?.file_type().is_dir() {
            return Err(invalid());
        }
        let mut cleanup = CreationCleanup(fresh.then(|| (dir.to_owned(), binding.to_owned(), len)));
        let owner_path = dir.join("owner");
        if !fresh {
            regular(&owner_path)?;
        }
        let mut owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(fresh)
            .open(&owner_path)?;
        owner.try_lock()?;
        if fresh {
            // Immutable intent is durable before data creation. An interrupted data or
            // identity write is discardable only with this exact binding and the lock.
            serde_json::to_writer(
                &mut owner,
                &Owner {
                    binding: binding.into(),
                    len,
                },
            )?;
            owner.sync_all()?;
            sync_dir(dir)?;
        }
        let data_path = dir.join("data");
        if !fresh {
            regular(&data_path)?;
        }
        let data = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(fresh)
            .open(&data_path)?;
        if fresh {
            data.set_len(len)?;
            data.sync_all()?;
            let mut id = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join("identity"))?;
            serde_json::to_writer(&mut id, &handle_identity(&data)?)?;
            id.sync_all()?;
            sync_dir(dir)?;
        } else {
            let mut bytes = Vec::new();
            owner.read_to_end(&mut bytes)?;
            let saved: Owner = serde_json::from_slice(&bytes)?;
            if saved.binding != binding
                || saved.len != len
                || data.metadata()?.len() != len
                || identity(dir)? != handle_identity(&data)?
            {
                return Err(invalid());
            }
        }
        cleanup.0 = None;
        Ok(Self {
            dir: dir.into(),
            _owner: owner,
            data,
        })
    }

    pub fn reopen(dir: &Path, binding: &str, len: u64) -> io::Result<Self> {
        // Never recreate a deleted journal's staging directory.
        Self::open_inner(dir, binding, len, false)
    }

    pub fn file_identity(&self) -> io::Result<String> {
        file_identity(&self.data)
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("data")
    }

    /// Validate the pathname again before installation: never install a replacement file.
    pub fn validate(&self) -> io::Result<()> {
        self.open_data().map(|_| ())
    }

    pub fn open_data(&self) -> io::Result<File> {
        regular(&self.path())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path())?;
        if handle_identity(&file)? != handle_identity(&self.data)? {
            return Err(invalid());
        }
        Ok(file)
    }
}

/// A read-only installed file proven by the journal's stable OS identity.
/// Hold any surviving owner lock until verification/completion has finished.
pub struct InstalledStage {
    path: PathBuf,
    identity: String,
    len: u64,
    _owner: Option<File>,
}

fn file_identity(file: &File) -> io::Result<String> {
    Ok(serde_json::to_string(&handle_identity(file)?)?)
}

impl InstalledStage {
    pub fn reopen(
        dir: &Path,
        binding: &str,
        len: u64,
        path: &Path,
        recorded: &str,
    ) -> io::Result<Self> {
        // Never bypass normal ownership validation while stage data still exists.
        match std::fs::symlink_metadata(dir.join("data")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
            Ok(_) => return Err(invalid()),
        }
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_dir() => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
            Ok(_) => return Err(invalid()),
        }
        let owner = match regular(&dir.join("owner")) {
            Ok(()) => {
                let mut owner = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(dir.join("owner"))?;
                owner.try_lock()?;
                let saved: Owner = serde_json::from_reader(&mut owner)?;
                if saved.binding != binding || saved.len != len {
                    return Err(invalid());
                }
                Some(owner)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let installed = Self {
            path: path.into(),
            identity: recorded.into(),
            len,
            _owner: owner,
        };
        installed.open_data()?;
        Ok(installed)
    }

    pub fn file_identity(&self) -> &str {
        &self.identity
    }

    /// Reopen and compare the handle, including at completion, never just the path.
    pub fn open_data(&self) -> io::Result<File> {
        regular(&self.path)?;
        let file = File::open(&self.path)?;
        if file_identity(&file)? != self.identity || file.metadata()?.len() != self.len {
            return Err(invalid());
        }
        Ok(file)
    }
}

/// Terminal cleanup keeps the journal row on any failure, including a live owner.
pub fn remove(dir: &Path, binding: &str, len: u64) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) if meta.file_type().is_dir() => (),
        Ok(_) => return Err(invalid()),
        Err(e) => return Err(e),
    }
    let owner_path = dir.join("owner");
    match regular(&owner_path) {
        // No manifest was created yet: only an empty directory is discardable.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return std::fs::remove_dir(dir),
        other => other?,
    }
    let mut owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&owner_path)?;
    owner.try_lock()?;
    let mut bytes = Vec::new();
    owner.read_to_end(&mut bytes)?;
    let saved = serde_json::from_slice::<Owner>(&bytes);
    match saved {
        Ok(saved) if saved.binding == binding && saved.len == len => (),
        Ok(_) => return Err(invalid()),
        Err(_) => {
            // A crash while writing intent precedes data creation. Accept only an
            // exact prefix of our intent and no other files; never guess a binding.
            let expected = serde_json::to_vec(&Owner {
                binding: binding.into(),
                len,
            })?;
            if !expected.starts_with(&bytes)
                || std::fs::read_dir(dir)?
                    .any(|entry| entry.map_or(true, |e| e.file_name() != "owner"))
            {
                return Err(invalid());
            }
        }
    }
    // An absent/truncated identity is an unfinished create, not resumable data.
    // A complete identity must match: replaced data is never discarded.
    let saved_identity = match identity(dir) {
        Ok(id) => Some(id),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::InvalidData
                || e.kind() == io::ErrorKind::UnexpectedEof =>
        {
            None
        }
        Err(e) => return Err(e),
    };
    match std::fs::symlink_metadata(dir.join("data")) {
        Ok(meta) if meta.file_type().is_file() => {
            let data = File::open(dir.join("data"))?;
            let actual_identity = handle_identity(&data)?;
            if saved_identity.is_some_and(|id| actual_identity != id) {
                return Err(invalid());
            }
            if saved_identity.is_none() {
                match std::fs::read(dir.join("identity")) {
                    Ok(bytes) if serde_json::to_vec(&actual_identity)?.starts_with(&bytes) => (),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    _ => return Err(invalid()),
                }
            }
            std::fs::remove_file(dir.join("data"))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (), // install already moved our data
        Ok(_) => return Err(invalid()),
        Err(e) => return Err(e),
    }
    match std::fs::remove_file(dir.join("identity")) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    std::fs::remove_file(dir.join("owner"))?;
    drop(owner);
    std::fs::remove_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    /// Advisory lock release is synchronous on every supported platform, but some
    /// filesystems expose the unlocked state to another opener a moment late.
    /// Every post-drop acquisition expected to succeed uses this bounded retry.
    fn after_release<T>(label: &str, mut operation: impl FnMut() -> io::Result<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match operation() {
                Ok(value) => return value,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{label} failed: {error}"),
            }
        }
    }

    fn assert_identity_rejected(result: io::Result<()>) {
        match result {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                panic!("identity rejection must not depend on held ownership: {error}")
            }
            Err(_) => (),
            Ok(()) => panic!("a different owner identity must be rejected"),
        }
    }

    #[test]
    fn native_path_locator_round_trips_without_unicode_replacement() {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"root-\xff/stage".to_vec()))
        };
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[
                b'C' as u16,
                58,
                92,
                0xd800,
                92,
                115,
            ]))
        };
        assert_eq!(decode_path(&encode_path(&path)).unwrap(), path);
        assert!(decode_path("not a native path locator").is_err());
    }

    #[test]
    fn owned_stage_lock_binding_and_interrupted_install_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stage");
        let stage = OwnedStage::create(&path, "job-entry-version-destination", 4).unwrap();
        let contention = match OwnedStage::reopen(&path, "job-entry-version-destination", 4) {
            Err(error) => error,
            Ok(_) => panic!("a live owner must hold an exclusive lock"),
        };
        assert_eq!(contention.kind(), io::ErrorKind::WouldBlock);
        assert!(remove(&path, "job-entry-version-destination", 4).is_err());
        drop(stage);
        assert_identity_rejected(OwnedStage::reopen(&path, "another identity", 4).map(|_| ()));
        assert_identity_rejected(remove(&path, "another identity", 4));
        let stage = after_release("reopen after release", || {
            OwnedStage::reopen(&path, "job-entry-version-destination", 4)
        });
        // A normal stage reopen rejects installed data. Terminal cleanup may
        // discard the manifest but never consults the final path.
        std::fs::rename(stage.path(), root.path().join("final")).unwrap();
        drop(stage);
        assert_identity_rejected(
            OwnedStage::reopen(&path, "job-entry-version-destination", 4).map(|_| ()),
        );
        after_release("terminal cleanup", || {
            remove(&path, "job-entry-version-destination", 4)
        });
        assert!(!path.exists());
        assert_eq!(std::fs::read(root.path().join("final")).unwrap(), [0; 4]);
        assert!(OwnedStage::reopen(&path, "job-entry-version-destination", 4).is_err());
        assert!(
            !path.exists(),
            "reopen must never resurrect deleted staging"
        );
    }
}
