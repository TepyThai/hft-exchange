//! Snapshots (week 18): the complete state at one sequence number, so
//! recovery replays only the journal tail after it.
//!
//! The journal is the source of truth; a snapshot is a cache of replaying
//! it up to `seq`. Restoring snapshot `s` and replaying `s + 1 ..` must give
//! exactly the state (and [`state_hash`](crate::StateMachine::state_hash))
//! of replaying everything — that is the property to test.
//!
//! # Files
//!
//! A [`SnapshotStore`] is a directory of `snapshot-<seq, 20 digits>.snap`
//! files, one per snapshot:
//!
//! ```text
//!   off  size  field
//!     0     8  magic   b"HFTSNAP1"
//!     8     8  seq     u64 LE   last command the state includes
//!    16     8  len     u64 LE   state length in bytes
//!    24     4  crc     u32 LE   CRC-32 of bytes [0, 24) ++ state
//!    28   len  state            Snapshottable::save_snapshot output
//! ```
//!
//! Writing is atomic: the file is written under a temporary name, synced,
//! renamed into place and the directory synced. A crash leaves either the
//! old set of snapshots or the new one, never a half-written snapshot under
//! a real name — and if one is damaged anyway, loading falls back to the
//! next older one.

use crate::replay::StateMachine;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use types::Seq;

/// A [`StateMachine`] that can save its complete state and load it back.
pub trait Snapshottable: StateMachine {
    /// Sequence number of the last command applied (`Seq(0)`: none) — the
    /// position in the journal the state corresponds to.
    fn applied_seq(&self) -> Seq;

    /// Appends a self-contained encoding of the complete state to `out`.
    fn save_snapshot(&self, out: &mut Vec<u8>);

    /// Replaces the whole state with one written by
    /// [`save_snapshot`](Snapshottable::save_snapshot). On error the state
    /// is unspecified: rebuild the machine.
    fn load_snapshot(&mut self, bytes: &[u8]) -> Result<(), SnapshotError>;
}

/// Why a snapshot could not be written or loaded.
#[derive(Debug)]
pub enum SnapshotError {
    /// The file system failed.
    Io(io::Error),
    /// A snapshot file is damaged or is not a snapshot.
    Corrupt { path: PathBuf, reason: &'static str },
    /// The file is intact but the state machine rejected its contents.
    State(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "snapshot i/o error: {e}"),
            SnapshotError::Corrupt { path, reason } => {
                write!(f, "snapshot {} is corrupt: {reason}", path.display())
            }
            SnapshotError::State(e) => write!(f, "snapshot state rejected: {e}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

/// A snapshot read back by [`SnapshotStore::load_latest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedSnapshot {
    /// Last command the state includes.
    pub seq: Seq,
    /// The state bytes, for [`Snapshottable::load_snapshot`].
    pub state: Vec<u8>,
    /// The file it came from.
    pub path: PathBuf,
    /// Newer snapshot files that were damaged and skipped.
    pub skipped: usize,
}

/// Magic number at the start of every snapshot file.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"HFTSNAP1";
/// Size of a snapshot file header.
pub const SNAPSHOT_HEADER_LEN: usize = 28;

/// A directory of snapshot files (format in the [module docs](self)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotStore {
    dir: PathBuf,
    retain: usize,
}

/// `snapshot-00000000000000001234.snap` → `Seq(1234)`.
fn parse_name(name: &str) -> Option<Seq> {
    let digits = name.strip_prefix("snapshot-")?.strip_suffix(".snap")?;
    (digits.len() == 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok().map(Seq))
        .flatten()
}

impl SnapshotStore {
    /// The store in `dir`, created if missing. Keeps the newest 2 snapshots
    /// (see [`retain`](SnapshotStore::retain)).
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<SnapshotStore> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(SnapshotStore { dir, retain: 2 })
    }

    /// Keep the newest `n` snapshots (at least 1) and delete older ones
    /// after each save. More than one guards against a damaged newest
    /// snapshot.
    pub fn retain(mut self, n: usize) -> SnapshotStore {
        self.retain = n.max(1);
        self
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the snapshot for `seq`.
    pub fn path_for(&self, seq: Seq) -> PathBuf {
        self.dir.join(format!("snapshot-{:020}.snap", seq.0))
    }

    /// Every snapshot file, oldest first (by name — contents unchecked).
    /// O(directory entries).
    pub fn list(&self) -> io::Result<Vec<(Seq, PathBuf)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if let Some(seq) = entry.file_name().to_str().and_then(parse_name) {
                out.push((seq, entry.path()));
            }
        }
        out.sort();
        Ok(out)
    }
}

// @student w18
impl SnapshotStore {
    /// Atomically writes the snapshot of `state` taken at `seq`, then
    /// deletes snapshots beyond the newest `retain`. Returns its path.
    ///
    /// Write to a temporary file, fsync it, rename it into place, fsync the
    /// directory: after a crash at any point the file is either absent or
    /// complete. O(state) plus two fsyncs.
    pub fn save(&self, seq: Seq, state: &[u8]) -> Result<PathBuf, SnapshotError> {
        use std::io::Write;
        let mut header = [0u8; SNAPSHOT_HEADER_LEN];
        header[0..8].copy_from_slice(&SNAPSHOT_MAGIC);
        header[8..16].copy_from_slice(&seq.0.to_le_bytes());
        header[16..24].copy_from_slice(&(state.len() as u64).to_le_bytes());
        let mut crc = crate::crc::Crc32::new();
        crc.update(&header[..24]);
        crc.update(state);
        header[24..28].copy_from_slice(&crc.finish().to_le_bytes());

        let path = self.path_for(seq);
        let tmp = self.dir.join(format!(".snapshot-{:020}.tmp", seq.0));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&header)?;
            f.write_all(state)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        crate::storage::sync_parent_dir(&path)?;

        let all = self.list()?;
        if all.len() > self.retain {
            for (_, old) in &all[..all.len() - self.retain] {
                std::fs::remove_file(old)?;
            }
        }
        Ok(path)
    }

    /// Reads and verifies the snapshot file at `path`: magic, length, CRC,
    /// and that the sequence number matches the file name. O(file).
    pub fn read(&self, path: &Path) -> Result<(Seq, Vec<u8>), SnapshotError> {
        let corrupt = |reason| SnapshotError::Corrupt {
            path: path.to_path_buf(),
            reason,
        };
        let bytes = std::fs::read(path)?;
        if bytes.len() < SNAPSHOT_HEADER_LEN {
            return Err(corrupt("shorter than the header"));
        }
        if bytes[0..8] != SNAPSHOT_MAGIC {
            return Err(corrupt("bad magic"));
        }
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"));
        let (seq, len) = (Seq(word(8)), word(16));
        if len != (bytes.len() - SNAPSHOT_HEADER_LEN) as u64 {
            return Err(corrupt("length does not match the file size"));
        }
        let stored = u32::from_le_bytes(bytes[24..28].try_into().expect("4 bytes"));
        let mut crc = crate::crc::Crc32::new();
        crc.update(&bytes[..24]);
        crc.update(&bytes[SNAPSHOT_HEADER_LEN..]);
        if crc.finish() != stored {
            return Err(corrupt("crc mismatch"));
        }
        let named = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_name);
        if named != Some(seq) {
            return Err(corrupt("sequence number does not match the file name"));
        }
        Ok((seq, bytes[SNAPSHOT_HEADER_LEN..].to_vec()))
    }

    /// The newest snapshot that verifies, skipping (and counting) damaged
    /// newer ones; `None` if there is no usable snapshot. I/O errors other
    /// than damage are returned. O(files tried).
    pub fn load_latest(&self) -> Result<Option<LoadedSnapshot>, SnapshotError> {
        let mut skipped = 0;
        for (_, path) in self.list()?.into_iter().rev() {
            match self.read(&path) {
                Ok((seq, state)) => {
                    return Ok(Some(LoadedSnapshot {
                        seq,
                        state,
                        path,
                        skipped,
                    }))
                }
                Err(SnapshotError::Corrupt { .. }) => skipped += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}
