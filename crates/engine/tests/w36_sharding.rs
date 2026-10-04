//! Week 36: the multi-instrument engine — registry, routing by instrument
//! and by order id, per-shard sequencing and determinism, shutdown.

use engine::testkit::{FlowConfig, OrderFlow};
use engine::{
    Engine, EngineConfig, InstrumentRegistry, OrderBook, RegistryError, RouteError, ShardEvent,
    ShardId, ShardedEngine,
};
use std::collections::BTreeMap;
use types::{
    Command, Event, InstrumentId, InstrumentSpec, NewOrder, OrderId, Qty, RejectReason, Rng, Seq,
    Side,
};

fn spec(id: u32) -> InstrumentSpec {
    InstrumentSpec {
        id: InstrumentId(id),
        ..InstrumentSpec::default()
    }
}

fn registry(instruments: u32, shards: usize) -> InstrumentRegistry {
    let mut r = InstrumentRegistry::new(shards);
    for i in 1..=instruments {
        r.register(spec(i)).unwrap();
    }
    r
}

/// Per-instrument w12 streams (ids made globally unique: `id * 64 + i`),
/// interleaved in a uniformly random order.
fn multi_stream(seed: u64, instruments: u32, n: usize) -> Vec<Command> {
    let mut flows: Vec<OrderFlow> = (1..=instruments)
        .map(|i| {
            OrderFlow::new(FlowConfig {
                seed: seed * 1_000 + u64::from(i),
                ..FlowConfig::default()
            })
        })
        .collect();
    let mut rng = Rng::new(seed);
    let remap = |id: OrderId, i: u32| OrderId(id.0 * 64 + u64::from(i));
    (0..n)
        .map(|_| {
            let i = rng.range(1, u64::from(instruments)) as u32;
            match flows[i as usize - 1].next_command() {
                Command::New(mut o) => {
                    o.id = remap(o.id, i);
                    o.instrument = InstrumentId(i);
                    Command::New(o)
                }
                Command::Cancel { id } => Command::Cancel { id: remap(id, i) },
                Command::Reduce { id, qty } => Command::Reduce {
                    id: remap(id, i),
                    qty,
                },
            }
        })
        .collect()
}

/// Runs `cmds` through a sharded engine, flushing every `flush_every`
/// commands. Returns the events, the commands the router accepted (with
/// the instrument they went to), and the engines after shutdown.
#[allow(clippy::type_complexity)]
fn run_sharded(
    reg: InstrumentRegistry,
    batch: usize,
    cmds: &[Command],
    flush_every: usize,
) -> (
    Vec<ShardEvent>,
    Vec<(InstrumentId, Command)>,
    Vec<(InstrumentId, Engine)>,
) {
    let mut sharded = ShardedEngine::new(reg, batch);
    let mut events = Vec::new();
    let mut routed = Vec::new();
    // Order id → instrument, as the test sees it (to label cancels).
    let mut owner: BTreeMap<OrderId, InstrumentId> = BTreeMap::new();
    for (i, cmd) in cmds.iter().enumerate() {
        if let Ok((shard, _)) = sharded.send(cmd) {
            let inst = match *cmd {
                Command::New(o) => {
                    owner.insert(o.id, o.instrument);
                    o.instrument
                }
                Command::Cancel { id } | Command::Reduce { id, .. } => owner[&id],
            };
            assert_eq!(sharded.registry().shard_of(inst), Some(shard));
            routed.push((inst, *cmd));
        }
        if (i + 1) % flush_every == 0 {
            sharded.flush(&mut events);
        }
    }
    let engines = sharded.shutdown(&mut events);
    (events, routed, engines)
}

/// The events of `shard`, in order.
fn of_shard(events: &[ShardEvent], shard: ShardId) -> Vec<ShardEvent> {
    events
        .iter()
        .filter(|e| e.shard == shard)
        .copied()
        .collect()
}

#[test]
fn w36_registry_assigns_shards_and_rejects_duplicates() {
    let mut r = InstrumentRegistry::new(2);
    assert!(r.is_empty());
    let shards: Vec<u16> = (1..=5).map(|i| r.register(spec(i)).unwrap().0).collect();
    assert_eq!(shards, vec![0, 1, 0, 1, 0], "balanced, lowest shard first");
    assert_eq!(
        r.register(spec(3)),
        Err(RegistryError::Duplicate(InstrumentId(3)))
    );
    assert_eq!(
        r.register_on(spec(9), ShardId(2)),
        Err(RegistryError::NoSuchShard(ShardId(2)))
    );
    assert_eq!(r.register_on(spec(9), ShardId(1)), Ok(ShardId(1)));
    assert_eq!(r.len(), 6);
    assert_eq!(r.shard_count(), 2);
    assert_eq!(r.shard_of(InstrumentId(4)), Some(ShardId(1)));
    assert_eq!(r.shard_of(InstrumentId(42)), None);
    assert_eq!(r.spec(InstrumentId(9)).map(|s| s.id), Some(InstrumentId(9)));
    let on1: Vec<u32> = r
        .instruments_on(ShardId(1))
        .iter()
        .map(|s| s.id.0)
        .collect();
    assert_eq!(on1, vec![2, 4, 9]);
    // 3 + 3: a tie goes to the lowest shard, then the emptier one.
    assert_eq!(r.register(spec(10)), Ok(ShardId(0)));
    assert_eq!(r.register(spec(11)), Ok(ShardId(1)));
    assert_eq!(r.register(spec(12)), Ok(ShardId(0)));
}

#[test]
fn w36_routes_by_instrument_and_cancels_through_the_id_map() {
    let mut r = InstrumentRegistry::new(2);
    let a = InstrumentId(1);
    let b = InstrumentId(2);
    assert_eq!(r.register(spec(1)), Ok(ShardId(0)));
    assert_eq!(r.register(spec(2)), Ok(ShardId(1)));
    let mut s = ShardedEngine::new(r, 8);
    let on = |i: InstrumentId, mut o: NewOrder| {
        o.instrument = i;
        Command::New(o)
    };
    // Per-shard sequence numbers start at 1 on each shard.
    assert_eq!(
        s.send(&on(a, NewOrder::limit(1, Side::Sell, 100, 5))),
        Ok((ShardId(0), Seq(1)))
    );
    assert_eq!(
        s.send(&on(b, NewOrder::limit(2, Side::Sell, 200, 5))),
        Ok((ShardId(1), Seq(1)))
    );
    assert_eq!(
        s.send(&on(a, NewOrder::limit(3, Side::Buy, 99, 5))),
        Ok((ShardId(0), Seq(2)))
    );
    // Cancels and reduces carry no instrument: routed by id.
    assert_eq!(
        s.send(&Command::Reduce {
            id: OrderId(2),
            qty: Qty(3)
        }),
        Ok((ShardId(1), Seq(2)))
    );
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(3) }),
        Ok((ShardId(0), Seq(3)))
    );
    // Router-level refusals: nothing queued, no sequence number used.
    assert_eq!(
        s.send(&on(InstrumentId(7), NewOrder::limit(4, Side::Buy, 1, 1))),
        Err(RouteError::UnknownInstrument(InstrumentId(7)))
    );
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(3) }),
        Err(RouteError::UnknownOrder(OrderId(3))),
        "a cancelled id is forgotten at once"
    );
    assert_eq!(
        s.send(&Command::Reduce {
            id: OrderId(99),
            qty: Qty(1)
        }),
        Err(RouteError::UnknownOrder(OrderId(99)))
    );
    assert_eq!(
        s.send(&on(b, NewOrder::limit(1, Side::Buy, 1, 1))),
        Err(RouteError::DuplicateOrderId(OrderId(1))),
        "id 1 is live on another instrument"
    );
    assert_eq!(s.shard_seq(ShardId(0)), Seq(3));
    assert_eq!(s.shard_seq(ShardId(1)), Seq(2));
    let mut events = Vec::new();
    s.flush(&mut events);
    let shard1: Vec<(Seq, Event)> = of_shard(&events, ShardId(1))
        .into_iter()
        .map(|e| (e.seq, e.event))
        .collect();
    assert_eq!(
        shard1,
        vec![
            (Seq(1), Event::Accepted { id: OrderId(2) }),
            (
                Seq(1),
                Event::Rested {
                    id: OrderId(2),
                    side: Side::Sell,
                    price: types::Price(200),
                    qty: Qty(5)
                }
            ),
            (
                Seq(2),
                Event::Reduced {
                    id: OrderId(2),
                    remaining: Qty(3)
                }
            ),
        ]
    );
    let cancelled = of_shard(&events, ShardId(0)).last().copied().unwrap();
    assert_eq!(cancelled.instrument, a);
    assert_eq!(
        cancelled.event,
        Event::Cancelled {
            id: OrderId(3),
            remaining: Qty(5)
        }
    );
}

#[test]
fn w36_each_instrument_matches_a_standalone_engine() {
    let cmds = multi_stream(0x36, 8, 40_000);
    let (events, routed, engines) = run_sharded(registry(8, 4), 64, &cmds, 5_000);
    // Most commands route; cancels of orders that ended before the last
    // flush are refused by the router (the generator does not track fills).
    assert!(routed.len() > cmds.len() / 2 && routed.len() < cmds.len());
    // Per shard: sequence numbers are gap-free, in order, one per routed
    // command (every command produces at least one event).
    for shard in 0..4u16 {
        let mut last = 0;
        for e in of_shard(&events, ShardId(shard)) {
            assert!(e.seq.0 == last || e.seq.0 == last + 1, "shard {shard}");
            last = e.seq.0;
        }
        // Round robin: instrument i lives on shard (i - 1) % 4.
        let routed_here = routed
            .iter()
            .filter(|(i, _)| (i.0 - 1) % 4 == u32::from(shard))
            .count();
        assert_eq!(last, routed_here as u64, "shard {shard}");
    }
    // Per instrument: the events of a standalone engine fed the same
    // commands, in the same order.
    let mut want: BTreeMap<InstrumentId, Vec<Event>> = BTreeMap::new();
    let mut solo: BTreeMap<InstrumentId, Engine> = BTreeMap::new();
    for (inst, cmd) in &routed {
        let e = solo
            .entry(*inst)
            .or_insert_with(|| Engine::with_config(EngineConfig::new(spec(inst.0))));
        e.process(cmd, want.entry(*inst).or_default());
    }
    let mut got: BTreeMap<InstrumentId, Vec<Event>> = BTreeMap::new();
    for e in &events {
        got.entry(e.instrument).or_default().push(e.event);
    }
    assert_eq!(got.len(), 8);
    for (inst, evs) in &want {
        assert_eq!(got[inst].len(), evs.len(), "{inst:?}");
        assert!(got[inst] == *evs, "{inst:?}: events differ");
    }
    let ids: Vec<u32> = engines.iter().map(|(i, _)| i.0).collect();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>(), "sorted by instrument");
    for (inst, engine) in &engines {
        assert_eq!(engine.state_hash(), solo[inst].state_hash(), "{inst:?}");
    }
}

#[test]
fn w36_shard_streams_are_deterministic_whatever_the_batching() {
    let cmds = multi_stream(0x37, 6, 30_000);
    let (a, ra, ea) = run_sharded(registry(6, 3), 1, &cmds, 2_000);
    let (b, rb, eb) = run_sharded(registry(6, 3), 512, &cmds, 2_000);
    assert_eq!(ra, rb, "routing decisions");
    for shard in 0..3u16 {
        assert_eq!(
            of_shard(&a, ShardId(shard)),
            of_shard(&b, ShardId(shard)),
            "shard {shard}"
        );
    }
    let ha: Vec<u64> = ea.iter().map(|(_, e)| e.state_hash()).collect();
    let hb: Vec<u64> = eb.iter().map(|(_, e)| e.state_hash()).collect();
    assert_eq!(ha, hb);
}

#[test]
fn w36_ended_orders_are_forgotten_at_flush() {
    let mut r = InstrumentRegistry::new(2);
    r.register(spec(1)).unwrap();
    r.register(spec(2)).unwrap();
    let mut s = ShardedEngine::new(r, 4);
    let on = |i: u32, mut o: NewOrder| {
        o.instrument = InstrumentId(i);
        Command::New(o)
    };
    let mut events = Vec::new();
    s.send(&on(1, NewOrder::limit(1, Side::Sell, 100, 5)))
        .unwrap();
    s.send(&on(1, NewOrder::limit(2, Side::Buy, 100, 5)))
        .unwrap();
    s.send(&on(1, NewOrder::limit(7, Side::Buy, 90, 1)))
        .unwrap();
    // 1 and 2 traded out, but the router does not know until a flush: a
    // cancel still goes to the shard, which rejects it.
    assert_eq!(s.routable_orders(), 3);
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(1) }).map(|r| r.0),
        Ok(ShardId(0))
    );
    // A same-instrument duplicate goes to the engine (which rejects it) and
    // keeps the live order routable; another instrument is refused.
    s.send(&on(1, NewOrder::limit(7, Side::Buy, 91, 1)))
        .unwrap();
    assert_eq!(
        s.send(&on(2, NewOrder::limit(7, Side::Buy, 91, 1))),
        Err(RouteError::DuplicateOrderId(OrderId(7)))
    );
    s.flush(&mut events);
    assert!(events.iter().any(|e| e.event
        == Event::Rejected {
            id: OrderId(1),
            reason: RejectReason::UnknownOrder
        }));
    assert!(events.iter().any(|e| e.event
        == Event::Rejected {
            id: OrderId(7),
            reason: RejectReason::DuplicateOrderId
        }));
    assert_eq!(s.routable_orders(), 1, "only order 7 is live");
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(2) }),
        Err(RouteError::UnknownOrder(OrderId(2)))
    );
    // A dead id may be reused, on any instrument.
    assert_eq!(
        s.send(&on(2, NewOrder::limit(2, Side::Sell, 300, 1)))
            .map(|r| r.0),
        Ok(ShardId(1))
    );
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(2) }).map(|r| r.0),
        Ok(ShardId(1))
    );
    assert_eq!(
        s.send(&Command::Cancel { id: OrderId(7) }).map(|r| r.0),
        Ok(ShardId(0))
    );
    events.clear();
    let engines = s.shutdown(&mut events);
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(engines.iter().all(|(_, e)| e.book().is_empty()));
}

#[test]
fn w36_shutdown_and_drop_stop_every_thread() {
    // Shutdown hands back every engine with all work applied, even work
    // still sitting in partial batches.
    let mut s = ShardedEngine::new(registry(4, 4), 1_000);
    for i in 1..=4u32 {
        let mut o = NewOrder::limit(u64::from(i), Side::Buy, 10, 1);
        o.instrument = InstrumentId(i);
        s.send(&Command::New(o)).unwrap();
    }
    let mut events = Vec::new();
    let engines = s.shutdown(&mut events);
    assert_eq!(engines.len(), 4);
    assert!(engines
        .iter()
        .all(|(_, e)| e.book().len() == 1 && e.seq() == Seq(1)));
    assert_eq!(events.len(), 8);
    // Dropping without shutdown must not hang or leak running threads.
    for _ in 0..20 {
        let mut s = ShardedEngine::new(registry(3, 3), 16);
        for k in 0..100u64 {
            let mut o = NewOrder::limit(k + 1, Side::Sell, 100 + (k % 5) as i64, 1);
            o.instrument = InstrumentId((k % 3) as u32 + 1);
            s.send(&Command::New(o)).unwrap();
        }
        drop(s);
    }
}
