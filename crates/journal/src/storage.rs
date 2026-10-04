//! Byte storage under the file-backed journal (week 18): a real file, or
//! memory for tests.
//!
//! The [`FileJournal`](crate::FileJournal) only ever appends, reads back,
//! syncs and (after a crash) truncates, so that is all [`Storage`] asks
//! for. Going through a trait instead of `std::fs::File` directly is what
//! lets tests count fsyncs and simulate a power loss ([`MemStorage`]).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

// TODO(w18): implement — see the week guide on tepy.dev/hft.

/// Append-only byte storage with an explicit durability barrier.
///
/// Bytes passed to [`append`](Storage::append) are visible to reads at
/// once, but survive a power loss only after [`sync`](Storage::sync)
/// returns.
pub trait Storage {
    /// Current size in bytes.
    fn size(&mut self) -> io::Result<u64>;

    /// Reads exactly `buf.len()` bytes starting at `offset`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Appends `bytes` at the end. Not durable until the next `sync`.
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Makes every byte appended so far (and the size) durable: one
    /// `fdatasync` on a file. The expensive call — from ~20 µs on NVMe with
    /// a write cache to milliseconds on spinning disks.
    fn sync(&mut self) -> io::Result<()>;

    /// Cuts the storage down to `len` bytes (dropping a torn tail). Durable
    /// after the next `sync`.
    fn truncate(&mut self, len: u64) -> io::Result<()>;

    /// Replaces `out` with everything from `offset` to the end. O(bytes
    /// read); reuses `out`'s allocation.
    fn read_from(&mut self, offset: u64, out: &mut Vec<u8>) -> io::Result<()> {
        let size = self.size()?;
        let len = size.saturating_sub(offset) as usize;
        out.clear();
        out.resize(len, 0);
        if len > 0 {
            self.read_at(offset, out)?;
        }
        Ok(())
    }
}

impl<S: Storage + ?Sized> Storage for &mut S {
    fn size(&mut self) -> io::Result<u64> {
        (**self).size()
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        (**self).append(bytes)
    }
    fn sync(&mut self) -> io::Result<()> {
        (**self).sync()
    }
    fn truncate(&mut self, len: u64) -> io::Result<()> {
        (**self).truncate(len)
    }
}

/// A journal file on disk.
///
/// Writes go through the kernel's page cache (they survive a process crash
/// as soon as `append` returns) and reach the disk on
/// [`sync`](Storage::sync), which is `fdatasync`: data plus the file size,
/// not the other metadata nobody needs for recovery.
#[derive(Debug)]
pub struct FileStorage {
    // TODO(w18): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FileStorage {
    /// Opens `path` for reading and appending, creating it if missing.
    ///
    /// A newly created file's directory entry is itself only durable once
    /// the directory is synced, so creation also fsyncs the parent
    /// directory (on Unix) — otherwise a crash could lose the whole file
    /// even though its contents were synced.
    pub fn open(path: impl AsRef<Path>) -> io::Result<FileStorage> {
        todo!("w18: open")
    }

    /// Where the file lives.
    pub fn path(&self) -> &Path {
        todo!("w18: path")
    }
}

#[allow(unused_variables)]
impl Storage for FileStorage {
    fn size(&mut self) -> io::Result<u64> {
        todo!("w18: size")
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        todo!("w18: read_at")
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        todo!("w18: append")
    }

    fn sync(&mut self) -> io::Result<()> {
        todo!("w18: sync")
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        todo!("w18: truncate")
    }
}

/// Fsyncs the directory holding `path`, making a create or rename in it
/// durable. A no-op where directories cannot be opened (Windows).
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    if cfg!(unix) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// In-memory [`Storage`] for tests: counts syncs and remembers how much has
/// been synced, so a test can see exactly what a power loss would leave.
///
/// Clones share the same bytes (an `Arc<Mutex<..>>`): keep one clone to
/// inspect the storage while the journal owns the other.
///
/// ```
/// use journal::{MemStorage, Storage};
/// let mut s = MemStorage::new();
/// s.append(b"abc").unwrap();
/// s.sync().unwrap();
/// s.append(b"de").unwrap();
/// assert_eq!((s.bytes(), s.durable_bytes(), s.syncs()), (b"abcde".to_vec(), b"abc".to_vec(), 1));
/// assert_eq!(s.power_loss().bytes(), b"abc".to_vec());
/// ```
#[derive(Clone, Debug, Default)]
pub struct MemStorage {
    inner: Arc<Mutex<MemState>>,
}

#[derive(Debug, Default)]
struct MemState {
    bytes: Vec<u8>,
    /// Length of the prefix covered by the last sync.
    synced: usize,
    syncs: u64,
    appends: u64,
}

impl MemStorage {
    /// Empty storage.
    pub fn new() -> MemStorage {
        MemStorage::default()
    }

    /// Storage holding `bytes`, all of them durable (e.g. a file image a
    /// test damaged on purpose).
    pub fn from_bytes(bytes: Vec<u8>) -> MemStorage {
        let synced = bytes.len();
        MemStorage {
            inner: Arc::new(Mutex::new(MemState {
                bytes,
                synced,
                ..MemState::default()
            })),
        }
    }

    fn state(&self) -> MutexGuard<'_, MemState> {
        // A panic while holding the lock cannot leave the bytes half
        // written in a way that matters to a test; keep going.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Everything appended — what survives a *process* crash (the page
    /// cache keeps unsynced writes).
    pub fn bytes(&self) -> Vec<u8> {
        self.state().bytes.clone()
    }

    /// The prefix covered by the last sync — what survives a *power loss*.
    pub fn durable_bytes(&self) -> Vec<u8> {
        let s = self.state();
        s.bytes[..s.synced].to_vec()
    }

    /// A new, independent storage holding only the durable bytes: the disk
    /// after the power came back.
    pub fn power_loss(&self) -> MemStorage {
        MemStorage::from_bytes(self.durable_bytes())
    }

    /// Number of [`sync`](Storage::sync) calls so far.
    pub fn syncs(&self) -> u64 {
        self.state().syncs
    }

    /// Number of [`append`](Storage::append) calls so far (write syscalls,
    /// for a file).
    pub fn appends(&self) -> u64 {
        self.state().appends
    }
}

impl Storage for MemStorage {
    fn size(&mut self) -> io::Result<u64> {
        Ok(self.state().bytes.len() as u64)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let s = self.state();
        let start = offset as usize;
        let end = start
            .checked_add(buf.len())
            .filter(|&e| e <= s.bytes.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(&s.bytes[start..end]);
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.state();
        s.bytes.extend_from_slice(bytes);
        s.appends += 1;
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        let mut s = self.state();
        s.synced = s.bytes.len();
        s.syncs += 1;
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        let mut s = self.state();
        let len = (len as usize).min(s.bytes.len());
        s.bytes.truncate(len);
        s.synced = s.synced.min(len);
        Ok(())
    }
}
