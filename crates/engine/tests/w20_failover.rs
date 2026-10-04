//! Week 20: failover to a hot standby engine, in-process (threads and
//! channels), with leases on a manual clock.

use engine::testkit::{FlowConfig, OrderFlow};
use engine::{Engine, Pipeline};
use journal::{link, Journal, ManualClock, Primary, ReplicationError, Standby, StandbyStats};
use std::thread;
use std::time::Duration;
use types::{Command, Seq};

/// `n` commands of the w12 generator, fed back through a shadow engine.
fn stream(cfg: FlowConfig, n: usize) -> Vec<Command> {
    let mut flow = OrderFlow::new(cfg);
    let mut shadow = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    (0..n)
        .map(|_| {
            let cmd = flow.next_command();
            out.clear();
            shadow.process(&cmd, &mut out);
            flow.observe(&cmd, &out);
            cmd
        })
        .collect()
}

/// State hash after each prefix of `cmds`.
fn hashes(cfg: &FlowConfig, cmds: &[Command]) -> Vec<u64> {
    let mut e = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    let mut v = vec![e.state_hash()];
    for c in cmds {
        out.clear();
        e.process(c, &mut out);
        v.push(e.state_hash());
    }
    v
}

#[test]
fn w20_failover_loses_no_acknowledged_command() {
    let cfg = FlowConfig {
        seed: 2020,
        ..FlowConfig::default()
    };
    let cmds = stream(cfg, 30_000);
    let expect = hashes(&cfg, &cmds);
    let clock = ManualClock::new(0);
    let lease = Duration::from_millis(5);
    let wait = Duration::from_secs(10);

    // The standby engine on its own thread. It grants the first lease as it
    // is created, then applies frames until the lease runs out with no word
    // from the primary — and only then takes over.
    let (plink, slink) = link();
    let standby = Standby::new(
        Journal::new(),
        Engine::with_config(cfg.engine_config()),
        slink,
        clock.clone(),
        lease,
    );
    let standby = thread::spawn(move || -> (Journal, Engine, StandbyStats) {
        let mut s = standby;
        loop {
            match s.recv(Duration::from_millis(1)) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(ReplicationError::Disconnected) => thread::sleep(Duration::from_micros(200)),
                Err(e) => panic!("{e}"),
            }
            if s.can_promote() {
                let stats = s.stats();
                let (journal, engine) = s.promote().expect("lease over");
                return (journal, engine, stats);
            }
        }
    });

    // The primary: engine v1 sequencing through the replicating journal.
    // A client is acknowledged only once the standby has the command; acks
    // are batched (every 16 commands, as a gateway would per read batch).
    let primary = Primary::new(Journal::new(), plink, clock.clone(), lease);
    let mut p = Pipeline::with_journal(primary, Engine::with_config(cfg.engine_config())).unwrap();
    let kill_at = 17_777;
    let mut acknowledged = Seq(0);
    for (i, cmd) in cmds[..kill_at].iter().enumerate() {
        let seq = p.process(cmd).unwrap().seq;
        if i % 16 == 15 {
            p.journal_mut().wait_acked(seq, wait).unwrap();
            acknowledged = seq;
        }
        assert_eq!(p.engine().state_hash(), expect[seq.0 as usize]);
    }
    // Mid-stream crash: a few more commands are sequenced and sent, but
    // their clients never hear back. Then the primary is gone.
    for cmd in &cmds[kill_at..kill_at + 7] {
        p.process(cmd).unwrap();
    }
    assert!(acknowledged.0 >= kill_at as u64 - 16);
    drop(p);

    // No promotion while the lease stands, however long we wait.
    thread::sleep(Duration::from_millis(30));
    assert!(
        !standby.is_finished(),
        "standby promoted before the lease expired"
    );
    clock.advance(5_000_000);
    let (journal, engine, stats) = standby.join().unwrap();

    // Every acknowledged command survived; the standby is exactly the
    // primary as of its last command.
    let last = journal.last_seq().unwrap();
    assert!(
        last >= acknowledged,
        "acknowledged {acknowledged:?}, standby has {last:?}"
    );
    assert!(last.0 <= kill_at as u64 + 7);
    assert_eq!(stats.applied, last.0);
    assert_eq!((stats.gaps, stats.corrupt), (0, 0));
    for frame in journal.iter() {
        let frame = frame.unwrap();
        assert_eq!(frame.command().unwrap(), cmds[frame.seq.0 as usize - 1]);
    }
    assert_eq!(engine.state_hash(), expect[last.0 as usize]);

    // The new primary carries on from there; the result is as if nothing
    // had happened.
    let mut p = Pipeline::with_journal(journal, engine).unwrap();
    for cmd in &cmds[last.0 as usize..] {
        p.process(cmd).unwrap();
    }
    assert_eq!(p.journal().last_seq(), Some(Seq(30_000)));
    assert_eq!(p.engine().state_hash(), expect[30_000]);
}
