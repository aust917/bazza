//! Import-owned staging leases. A later import can reclaim a stage left by a
//! crashed process without touching active imports or unrelated recording data.

use crate::{Result, ShareError};
use hark_store::Store;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const PREFIX: &str = ".hark-import-";
const MARKER: &str = ".hark-import-stage";
const MAGIC: &str = "hark-import-stage-v1";

pub struct ImportStage {
    // Keep the lease alive while TempDir removes its owned files on drop.
    _directory: tempfile::TempDir,
    _lease: File,
    media: PathBuf,
}

impl ImportStage {
    pub fn new(store: &Store) -> Result<Self> {
        let recordings = store.root().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let recordings = recordings.canonicalize()?;
        reclaim_abandoned(&recordings)?;
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(&recordings)?;
        let name = directory.path().file_name().and_then(|s|s.to_str())
            .ok_or_else(||ShareError::Other("invalid import staging directory".into()))?;
        let mut lease = OpenOptions::new().read(true).write(true).create_new(true)
            .open(directory.path().join(MARKER))?;
        lease.try_lock().map_err(|e|ShareError::Other(format!("could not lease import staging directory: {e}")))?;
        lease.write_all(format!("{MAGIC}\n{name}\n").as_bytes())?;
        lease.sync_all()?;
        let media = directory.path().join("media");
        std::fs::create_dir(&media)?;
        Ok(Self { _directory:directory,_lease:lease,media })
    }

    /// Only media is handed to the store; the lease stays in its parent.
    pub fn path(&self) -> &Path { &self.media }
}

fn reclaim_abandoned(recordings: &Path) -> Result<()> {
    for entry in std::fs::read_dir(recordings)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name.strip_prefix(PREFIX) else { continue };
        if suffix.is_empty() || !suffix.bytes().all(|b|b.is_ascii_alphanumeric()) { continue; }
        let kind = entry.file_type()?;
        let path = entry.path();
        // Reject symlinks/junctions and require the exact intended absolute path.
        if !kind.is_dir() || kind.is_symlink() || path.canonicalize().ok().as_deref() != Some(path.as_path()) { continue; }
        let marker = path.join(MARKER);
        let Ok(metadata) = std::fs::symlink_metadata(&marker) else { continue };
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024
            || marker.canonicalize().ok().as_deref() != Some(marker.as_path()) { continue; }
        let Ok(mut lease) = OpenOptions::new().read(true).write(true).open(&marker) else { continue };
        if lease.try_lock().is_err() { continue; } // another importer still owns it
        let mut content = String::new();
        (&mut lease).take(1025).read_to_string(&mut content)?;
        if content != format!("{MAGIC}\n{name}\n") { continue; }
        std::fs::remove_dir_all(&path).map_err(|e|ShareError::Other(format!("could not reclaim abandoned import stage: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_stages_are_reclaimed_without_deleting_active_or_unrelated_data() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let active = ImportStage::new(&store).unwrap();
        std::fs::write(active.path().join("mix.wav"),b"active").unwrap();
        let abandoned = ImportStage::new(&store).unwrap();
        std::fs::write(abandoned.path().join("mix.wav"),b"abandoned").unwrap();
        // Emulate process interruption: preserve TempDir, then release its OS
        // lease without running the normal TempDir destructor.
        let ImportStage { _directory,_lease,media:_ } = abandoned;
        let abandoned_path = _directory.keep();
        drop(_lease);
        let unrelated = store.root().join("recordings/.hark-import-unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("mix.wav"),b"unrelated").unwrap();
        let next = ImportStage::new(&store).unwrap();
        assert!(!abandoned_path.exists());
        assert_eq!(std::fs::read(active.path().join("mix.wav")).unwrap(),b"active");
        assert_eq!(std::fs::read(unrelated.join("mix.wav")).unwrap(),b"unrelated");
        let next_path = next._directory.path().to_path_buf();
        drop(next);
        assert!(!next_path.exists(),"ordinary completion/error drops its own staging");
    }

    #[test]
    fn invalid_ownership_marker_does_not_authorize_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let stage = ImportStage::new(&store).unwrap();
        let recordings = stage._directory.path().parent().unwrap();
        let unrelated = recordings.join(".hark-import-other");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join(MARKER),format!("{MAGIC}\nwrong-name\n")).unwrap();
        std::fs::write(unrelated.join("mix.wav"),b"preserve").unwrap();
        let _next = ImportStage::new(&store).unwrap();
        assert_eq!(std::fs::read(unrelated.join("mix.wav")).unwrap(),b"preserve");
    }
}
