//! Week 18: the file-backed journal — fsync policies, torn tails vs
//! corruption, the sparse index, snapshots and generic recovery.

use journal::{
    recover, Clock, CorruptionKind, FileJournal, FileStorage, Frames, Journal, JournalError,
    JournalOptions, ManualClock, MemStorage, RecoveryError, SnapshotStore, Snapshottable,
    StateMachine, Storage, SyncPolicy, SystemClock, FIRST_SEQ,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use types::{Command, NewOrder, OrderId, Qty, Rng, Seq, Side};

// ---------------------------------------------------------------------------
// Helpers

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hft-w18-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Commands of all three sizes (34, 9 and 17 byte payloads).
fn commands(seed: u64, n: usize) -> Vec<Command> {
    let mut rng = Rng::new(seed);
    (1..=n as u64)
        .map(|i| match rng.range(0, 5) {
            0 => Command::Cancel {
                id: OrderId(rng.range(1, i)),
            },
            1 => Command::Reduce {
                id: OrderId(rng.range(1, i)),
                qty: Qty(rng.range(1, 50)),
            },
            _ => Command::New(NewOrder::limit(
                i,
                rng.side(),
                rng.range_i64(900, 1100),
                rng.range(1, 100),
            )),
        })
        .collect()
}

/// The bytes a journal of `cmds` holds (the file format is the in-memory
/// format).
fn journal_bytes(cmds: &[Command]) -> Vec<u8> {
    let mut j = Journal::new();
    for c in cmds {
        j.append(c);
    }
    j.into_bytes()
}

/// Byte offset of every frame, plus the total length at the end.
fn frame_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut v: Vec<usize> = Frames::new(bytes, FIRST_SEQ)
        .map(|f| f.unwrap().offset)
        .collect();
    v.push(bytes.len());
    v
}

fn never() -> JournalOptions {
    JournalOptions::with_sync(SyncPolicy::Never)
}

fn open_mem(
    storage: MemStorage,
    opts: JournalOptions,
) -> Result<FileJournal<MemStorage, ManualClock>, JournalError> {
    FileJournal::open(storage, opts, ManualClock::new(0)).map(|(j, _)| j)
}

fn read_all(j: &mut FileJournal<impl Storage, impl Clock>) -> Vec<Command> {
    let mut buf = Vec::new();
    j.iter_from(FIRST_SEQ, &mut buf)
        .unwrap()
        .map(|f| f.unwrap().command().unwrap())
        .collect()
}

/// A toy snapshottable machine: an order-sensitive digest of the commands.
#[derive(Clone, Debug, Default, PartialEq)]
struct Digest {
    seq: u64,
    h: u64,
}

impl StateMachine for Digest {
    fn apply(&mut self, cmd: &Command) {
        let x = match *cmd {
            Command::New(o) => o.id.0 ^ (o.qty.0 << 32),
            Command::Cancel { id } => id.0.rotate_left(17),
            Command::Reduce { id, qty } => id.0 ^ qty.0.rotate_left(40),
        };
        self.seq += 1;
        self.h = (self.h ^ x)
            .wrapping_mul(0x0000_0100_0000_01b3)
            .rotate_left(7);
    }

    fn state_hash(&self) -> u64 {
        self.h ^ self.seq.rotate_left(32)
    }
}

impl Snapshottable for Digest {
    fn applied_seq(&self) -> Seq {
        Seq(self.seq)
    }

    fn save_snapshot(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.h.to_le_bytes());
    }

    fn load_snapshot(&mut self, bytes: &[u8]) -> Result<(), journal::SnapshotError> {
        let b: [u8; 16] = bytes
            .try_into()
            .map_err(|_| journal::SnapshotError::State("digest snapshot is 16 bytes".into()))?;
        self.seq = u64::from_le_bytes(b[..8].try_into().unwrap());
        self.h = u64::from_le_bytes(b[8..].try_into().unwrap());
        Ok(())
    }
}

fn digest_of(cmds: &[Command]) -> Digest {
    let mut d = Digest::default();
    cmds.iter().for_each(|c| d.apply(c));
    d
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

// ---------------------------------------------------------------------------
// Tests

#[test]
fn w18_torn_write_at_every_offset_of_the_last_frame_recovers() {
    let dir = TempDir::new("torn");
    let path = dir.join("journal.log");
    let cmds = commands(18, 60);
    let n = cmds.len() as u64;

    // Write through the file journal itself.
    {
        let (mut j, opened) = FileJournal::open_path(&path, never()).unwrap();
        assert_eq!((opened.frames, opened.torn_tail), (0, None));
        for c in &cmds {
            j.append(c).unwrap();
        }
        j.sync().unwrap();
        assert_eq!(j.durable_seq(), Seq(n));
    }
    let full = std::fs::read(&path).unwrap();
    assert_eq!(
        full,
        journal_bytes(&cmds),
        "the file holds exactly the frames"
    );
    let offsets = frame_offsets(&full);
    let last = offsets[offsets.len() - 2];

    let extra = Command::New(NewOrder::limit(10_000, Side::Buy, 1_000, 7));
    for cut in last..full.len() {
        std::fs::write(&path, &full[..cut]).unwrap();
        let (mut j, opened) = FileJournal::open_path(&path, never()).unwrap();
        assert_eq!(opened.frames, n - 1, "cut at {cut}");
        assert_eq!(opened.bytes, last as u64);
        if cut == last {
            assert_eq!(opened.torn_tail, None, "a clean frame boundary is not torn");
        } else {
            let torn = opened.torn_tail.expect("partial last frame");
            assert!(torn.at.is_torn_tail());
            assert_eq!(torn.at.offset, last);
            assert_eq!(torn.dropped_bytes, (cut - last) as u64);
        }
        assert_eq!(file_len(&path), last as u64, "torn tail truncated on disk");
        // Appends continue where the valid prefix ends.
        assert_eq!(j.append(&extra).unwrap(), Seq(n));
        j.sync().unwrap();
        drop(j);
        let (mut j, opened) = FileJournal::open_path(&path, never()).unwrap();
        assert_eq!((opened.frames, opened.torn_tail), (n, None));
        let back = read_all(&mut j);
        assert_eq!(&back[..n as usize - 1], &cmds[..n as usize - 1]);
        assert_eq!(back[n as usize - 1], extra);
    }

    // A last frame at full length whose data never landed (zeros), and a
    // preallocated zero tail after the last frame: both torn, both cut.
    let mut zeroed = full.clone();
    zeroed[last + 4..].fill(0);
    let mut padded = full.clone();
    padded.extend_from_slice(&[0u8; 4096]);
    for (image, frames, kept) in [(zeroed, n - 1, last), (padded, n, full.len())] {
        std::fs::write(&path, &image).unwrap();
        let (_, opened) = FileJournal::open_path(&path, never()).unwrap();
        assert_eq!(opened.frames, frames);
        assert!(opened.torn_tail.is_some());
        assert_eq!(file_len(&path), kept as u64);
    }
}

#[test]
fn w18_mid_file_bit_flip_is_detected() {
    let cmds = commands(1818, 40);
    let bytes = journal_bytes(&cmds);
    let offsets = frame_offsets(&bytes);
    let frames = cmds.len();

    // Every bit of the first, a middle and the second-to-last frame.
    for k in [0, 17, frames - 2] {
        let (start, end) = (offsets[k], offsets[k + 1]);
        for byte in start..end {
            for bit in 0..8 {
                let mut damaged = bytes.clone();
                damaged[byte] ^= 1 << bit;
                let storage = MemStorage::from_bytes(damaged.clone());
                match open_mem(storage.clone(), never()) {
                    Err(JournalError::Corrupt { at, resumes_at }) => {
                        assert_eq!(at.offset, start, "frame {k}, byte {byte}, bit {bit}");
                        assert_eq!(resumes_at, end as u64, "frame {k}, byte {byte}, bit {bit}");
                    }
                    other => panic!("frame {k}, byte {byte}, bit {bit}: {other:?}"),
                }
                assert_eq!(
                    storage.bytes(),
                    damaged,
                    "corruption must not be truncated away"
                );
            }
        }
    }

    // The classic trap: a length field that now points past the end of the
    // file looks exactly like a torn write until you look further.
    let mut long = bytes.clone();
    long[offsets[17]..offsets[17] + 4].copy_from_slice(&60_000u32.to_le_bytes());
    match open_mem(MemStorage::from_bytes(long), never()) {
        Err(JournalError::Corrupt { at, resumes_at }) => {
            assert!(matches!(at.kind, CorruptionKind::Truncated { .. }));
            assert_eq!(resumes_at, offsets[18] as u64);
        }
        other => panic!("{other:?}"),
    }

    // Damage to the *last* frame is indistinguishable from a torn write.
    let mut last = bytes.clone();
    last[offsets[frames - 1] + 20] ^= 0x10;
    let (_, opened) =
        FileJournal::open(MemStorage::from_bytes(last), never(), SystemClock::new()).unwrap();
    assert_eq!(opened.frames, frames as u64 - 1);

    // The same on a real file: open fails and the file is left alone.
    let dir = TempDir::new("flip");
    let path = dir.join("journal.log");
    let mut damaged = bytes.clone();
    damaged[offsets[5] + 30] ^= 0x01;
    std::fs::write(&path, &damaged).unwrap();
    assert!(matches!(
        FileJournal::open_path(&path, never()),
        Err(JournalError::Corrupt { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
}

#[test]
fn w18_fsync_policy_is_honoured() {
    let cmds = commands(7, 1_000);

    // EveryN(1): every append is durable before it returns.
    let storage = MemStorage::new();
    let mut j = open_mem(
        storage.clone(),
        JournalOptions::with_sync(SyncPolicy::EveryN(1)),
    )
    .unwrap();
    for (i, c) in cmds[..10].iter().enumerate() {
        let seq = j.append(c).unwrap();
        assert_eq!(j.durable_seq(), seq);
        assert_eq!(storage.syncs(), i as u64 + 1);
    }
    assert_eq!(j.syncs(), 10);

    // EveryN(8): group commit; a power loss keeps whole groups only.
    let storage = MemStorage::new();
    let mut j = open_mem(
        storage.clone(),
        JournalOptions::with_sync(SyncPolicy::EveryN(8)),
    )
    .unwrap();
    for c in &cmds[..30] {
        j.append(c).unwrap();
    }
    assert_eq!((storage.syncs(), j.durable_seq()), (3, Seq(24)));
    let mut after = open_mem(storage.power_loss(), never()).unwrap();
    assert_eq!(read_all(&mut after), cmds[..24].to_vec());
    j.sync().unwrap();
    assert_eq!((storage.syncs(), j.durable_seq()), (4, Seq(30)));
    j.sync().unwrap();
    assert_eq!(storage.syncs(), 4, "nothing new to sync: no fsync");

    // Interval(1 ms) on a manual clock.
    let clock = ManualClock::new(0);
    let storage = MemStorage::new();
    let opts = JournalOptions::with_sync(SyncPolicy::Interval(Duration::from_millis(1)));
    let (mut j, _) = FileJournal::open(storage.clone(), opts, clock.clone()).unwrap();
    for c in &cmds[..5] {
        j.append(c).unwrap();
    }
    assert_eq!((storage.syncs(), j.durable_seq()), (0, Seq(0)));
    clock.advance(1_000_000);
    j.append(&cmds[5]).unwrap();
    assert_eq!((storage.syncs(), j.durable_seq()), (1, Seq(6)));
    for c in &cmds[6..9] {
        j.append(c).unwrap();
    }
    clock.advance(999_999);
    assert!(!j.poll().unwrap(), "interval not over yet");
    clock.advance(1);
    assert!(j.poll().unwrap());
    assert_eq!((storage.syncs(), j.durable_seq()), (2, Seq(9)));
    clock.advance(5_000_000);
    assert!(!j.poll().unwrap(), "nothing unsynced");

    // Never: writes reach the storage when the buffer fills, never synced.
    let storage = MemStorage::new();
    let opts = JournalOptions {
        buffer_bytes: 256,
        ..never()
    };
    let mut j = open_mem(storage.clone(), opts).unwrap();
    for c in &cmds {
        j.append(c).unwrap();
    }
    assert_eq!((storage.syncs(), j.durable_seq()), (0, Seq(0)));
    assert!(
        storage.appends() > 50,
        "the buffer was written out as it filled"
    );
    assert!(storage.durable_bytes().is_empty());
    let written = storage.bytes();
    assert!(!written.is_empty() && (written.len() as u64) < j.byte_len());
    // A process crash keeps what was written; a power loss keeps nothing.
    let mut crashed = open_mem(MemStorage::from_bytes(written.clone()), never()).unwrap();
    let survived = read_all(&mut crashed);
    assert_eq!(survived, cmds[..survived.len()].to_vec());
    assert!(open_mem(storage.power_loss(), never()).unwrap().is_empty());
}

#[test]
fn w18_sparse_index_seeks_without_scanning() {
    let cmds = commands(64, 10_000);
    let bytes = journal_bytes(&cmds);
    let offsets = frame_offsets(&bytes);
    let opts = JournalOptions {
        index_stride: 64,
        ..never()
    };
    let storage = MemStorage::new();
    let mut j = open_mem(storage.clone(), opts).unwrap();
    for c in &cmds {
        j.append(c).unwrap();
    }
    assert_eq!(j.index().len(), 10_000usize.div_ceil(64));
    assert_eq!(j.index().stride(), 64);

    let mut buf = Vec::new();
    for from in [1u64, 2, 63, 64, 65, 128, 129, 5_000, 9_999, 10_000, 10_001] {
        let (entry, offset) = j.index().lookup(Seq(from.min(10_000))).unwrap();
        assert_eq!(entry.0, 1 + (from.min(10_000) - 1) / 64 * 64, "from {from}");
        assert_eq!(offset, offsets[entry.0 as usize - 1] as u64);
        let got: Vec<(Seq, Command)> = j
            .iter_from(Seq(from), &mut buf)
            .unwrap()
            .map(|f| {
                let f = f.unwrap();
                (f.seq, f.command().unwrap())
            })
            .collect();
        let want: Vec<(Seq, Command)> = (from..=10_000)
            .map(|s| (Seq(s), cmds[s as usize - 1]))
            .collect();
        assert!(got == want, "from {from}");
        // Only the bytes from the indexed frame on were read — at most
        // 63 unwanted frames, never the whole prefix.
        assert_eq!(buf.len(), bytes.len() - offset as usize, "from {from}");
        assert!(from < 64 || offset as usize >= offsets[from as usize - 64]);
    }
    assert!(matches!(
        j.iter_from(Seq(0), &mut buf),
        Err(JournalError::OutOfRange { .. })
    ));
    assert!(matches!(
        j.iter_from(Seq(10_002), &mut buf),
        Err(JournalError::OutOfRange { .. })
    ));

    // Reading flushes buffered appends; reopening rebuilds the same index.
    j.append(&cmds[0]).unwrap();
    assert_eq!(j.iter_from(Seq(10_001), &mut buf).unwrap().count(), 1);
    let index = j.index().clone();
    let reopened = open_mem(MemStorage::from_bytes(storage.bytes()), opts).unwrap();
    assert_eq!(reopened.index(), &index);
    assert_eq!(reopened.next_seq(), Seq(10_002));
}

#[test]
fn w18_snapshots_are_atomic_verified_and_recovery_uses_them() {
    let dir = TempDir::new("snap");
    let store = SnapshotStore::open(dir.join("snapshots"))
        .unwrap()
        .retain(3);
    let cmds = commands(99, 3_000);
    let log = dir.join("journal.log");
    let opts = JournalOptions {
        index_stride: 128,
        ..JournalOptions::with_sync(SyncPolicy::EveryN(100))
    };

    // Journal the commands, snapshotting the machine every 500.
    let (mut j, _) = FileJournal::open_path(&log, opts).unwrap();
    let mut live = Digest::default();
    for (i, c) in cmds.iter().enumerate() {
        j.append(c).unwrap();
        live.apply(c);
        if (i + 1) % 500 == 0 {
            j.sync().unwrap();
            let mut state = Vec::new();
            live.save_snapshot(&mut state);
            store.save(live.applied_seq(), &state).unwrap();
        }
    }
    j.sync().unwrap();
    drop(j);
    let kept: Vec<Seq> = store.list().unwrap().into_iter().map(|(s, _)| s).collect();
    assert_eq!(kept, vec![Seq(2_000), Seq(2_500), Seq(3_000)], "retain 3");

    // Recovery from the newest snapshot equals a full replay, and replays
    // nothing when the snapshot is at the end.
    let mut m = Digest::default();
    let (_, rep) = recover(
        FileStorage::open(&log).unwrap(),
        Some(&store),
        opts,
        SystemClock::new(),
        &mut m,
    )
    .unwrap();
    assert_eq!(
        (rep.snapshot, rep.replayed, rep.applied),
        (Some(Seq(3_000)), 0, Seq(3_000))
    );
    assert_eq!(m, live);

    // A damaged newest snapshot is skipped in favour of the previous one;
    // a leftover temporary file is ignored.
    let newest = store.path_for(Seq(3_000));
    let mut raw = std::fs::read(&newest).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0xFF;
    std::fs::write(&newest, &raw).unwrap();
    std::fs::write(
        store.dir().join(".snapshot-00000000000000003001.tmp"),
        b"half",
    )
    .unwrap();
    let loaded = store.load_latest().unwrap().unwrap();
    assert_eq!((loaded.seq, loaded.skipped), (Seq(2_500), 1));
    let mut m = Digest::default();
    let (mut j, rep) = recover(
        FileStorage::open(&log).unwrap(),
        Some(&store),
        opts,
        SystemClock::new(),
        &mut m,
    )
    .unwrap();
    assert_eq!(
        (rep.snapshot, rep.skipped_snapshots, rep.replayed),
        (Some(Seq(2_500)), 1, 500)
    );
    assert_eq!(m, live, "snapshot + tail = everything");
    assert_eq!(m, digest_of(&cmds));
    assert_eq!(j.next_seq(), Seq(3_001));

    // A snapshot ahead of the journal (journal data lost) stops recovery.
    let mut ahead = Digest::default();
    for c in &cmds {
        ahead.apply(c);
    }
    ahead.apply(&cmds[0]);
    let mut state = Vec::new();
    ahead.save_snapshot(&mut state);
    store.save(Seq(3_001), &state).unwrap();
    j.sync().unwrap();
    drop(j);
    let err = recover(
        FileStorage::open(&log).unwrap(),
        Some(&store),
        opts,
        SystemClock::new(),
        &mut Digest::default(),
    );
    assert!(matches!(
        err,
        Err(RecoveryError::SnapshotAhead {
            snapshot: Seq(3_001),
            journal_next: Seq(3_001)
        })
    ));
}
