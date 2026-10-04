//! Week 13: the engine thread's logic, without the thread.
//!
//! [`Sequencer`] owns one [`Engine`] per instrument, the [`OrderRouter`]
//! and the [`Hooks`]. It turns what the sessions send — logons, commands
//! in client ids, disconnects — into engine commands in exchange ids, and
//! engine events back into per-session reports. Everything it does is a
//! deterministic function of the order of its inputs: run the same inputs
//! through two sequencers and they assign the same ids and produce the same
//! reports. The gateway's engine thread is a loop around it, and a test
//! can drive it directly.
//!
//! Per command:
//!
//! * **New order:** unknown instrument → `Rejected(Halted)`; client id
//!   already live in this session → `Rejected(DuplicateOrderId)`; pre-trade
//!   reject → `Rejected(reason)`. None of these reach the engine. Otherwise
//!   the order gets the next exchange id and is matched.
//! * **Cancel / reduce:** the client id is resolved within the session;
//!   unknown → `Rejected(UnknownOrder)` without touching the engine.
//! * **Disconnect:** with cancel-on-disconnect, every live order of the
//!   session is cancelled through the engine (ascending exchange id), so
//!   the journal explains each cancel.

use crate::hooks::Hooks;
use crate::router::OrderRouter;
use crate::session::SessionId;
use engine::{Engine, EngineConfig};
#[allow(unused_imports)] // used by the implementation the learner writes
use types::{AccountId, Command, Event, InstrumentId, NewOrder, OrderId, RejectReason};
#[allow(unused_imports)] // used by the implementation the learner writes
use wire::LogonAck;
use wire::Message;

/// Sequences session traffic through the engines (see the module docs).
// @student w13
pub struct Sequencer {
    /// `(instrument, engine)`, few enough that a linear scan beats a map.
    engines: Vec<(InstrumentId, Engine)>,
    router: OrderRouter,
    hooks: Hooks,
    cancel_on_disconnect: bool,
    /// Scratch for one command's engine events (reused, never shrinks).
    events: Vec<Event>,
    sequenced: u64,
}

// @student w13
impl Sequencer {
    /// One engine per entry of `instruments` (their `spec.id`s must be
    /// distinct). Allocates the engines.
    pub fn new(
        instruments: &[EngineConfig],
        hooks: Hooks,
        cancel_on_disconnect: bool,
    ) -> Sequencer {
        Sequencer {
            engines: instruments
                .iter()
                .map(|cfg| (cfg.spec.id, Engine::with_config(*cfg)))
                .collect(),
            router: OrderRouter::new(),
            hooks,
            cancel_on_disconnect,
            events: Vec::with_capacity(256),
            sequenced: 0,
        }
    }

    /// A session logged on: delivers its `LogonAck` (before any report).
    pub fn open<F: FnMut(SessionId, Message)>(
        &mut self,
        session: SessionId,
        account: AccountId,
        deliver: &mut F,
    ) {
        deliver(
            session,
            Message::LogonAck(LogonAck {
                account,
                session: session.0,
            }),
        );
    }

    /// Handles one command from `session` (client order ids), received at
    /// `rx_ns`, delivering every resulting report. O(events) plus the
    /// engine's own cost; allocates only when a table grows.
    pub fn command<F: FnMut(SessionId, Message)>(
        &mut self,
        session: SessionId,
        rx_ns: u64,
        cmd: &Command,
        deliver: &mut F,
    ) {
        let mut reject = |id: OrderId, reason: RejectReason| {
            deliver(session, Message::Event(Event::Rejected { id, reason }));
        };
        let (instrument, cmd) = match *cmd {
            Command::New(order) => {
                let Some(ix) = self.engine_index(order.instrument) else {
                    return reject(order.id, RejectReason::Halted);
                };
                if self.router.lookup(session, order.id).is_some() {
                    return reject(order.id, RejectReason::DuplicateOrderId);
                }
                let exchange = NewOrder {
                    id: self.router.next_id(),
                    ..order
                };
                let reference = self.engines[ix].1.reference_price();
                if let Err(reason) = self
                    .hooks
                    .pretrade
                    .check(session, &exchange, rx_ns, reference)
                {
                    return reject(order.id, reason);
                }
                let id = self
                    .router
                    .insert(session, order.id, order.instrument, order.qty)
                    .expect("client id checked above");
                debug_assert_eq!(id, exchange.id);
                (order.instrument, Command::New(exchange))
            }
            Command::Cancel { id } => match self.resolve(session, id) {
                Some((xid, instrument)) => (instrument, Command::Cancel { id: xid }),
                None => return reject(id, RejectReason::UnknownOrder),
            },
            Command::Reduce { id, qty } => match self.resolve(session, id) {
                Some((xid, instrument)) => (instrument, Command::Reduce { id: xid, qty }),
                None => return reject(id, RejectReason::UnknownOrder),
            },
        };
        self.apply(instrument, &cmd, deliver);
    }

    /// `session` disconnected: with cancel-on-disconnect, cancels its live
    /// orders through the engine (reports are still delivered, the shell
    /// drops them if the session is gone); tells the pre-trade hook.
    pub fn close<F: FnMut(SessionId, Message)>(&mut self, session: SessionId, deliver: &mut F) {
        if self.cancel_on_disconnect {
            for id in self.router.open_orders(session) {
                if let Some(instrument) = self.router.route(id).map(|r| r.instrument) {
                    self.apply(instrument, &Command::Cancel { id }, deliver);
                }
            }
        }
        self.hooks.pretrade.on_close(session);
    }

    /// End of a batch: lets the pipeline stage flush (e.g. fsync) before
    /// the batch's reports are released.
    pub fn flush(&mut self) {
        self.hooks.stage.flush();
    }

    /// Commands applied to the engines so far (pre-trade and gateway
    /// rejects excluded).
    pub fn sequenced(&self) -> u64 {
        self.sequenced
    }

    /// The engine of `instrument`.
    pub fn engine(&self, instrument: InstrumentId) -> Option<&Engine> {
        self.engine_index(instrument).map(|ix| &self.engines[ix].1)
    }

    /// The id map.
    pub fn router(&self) -> &OrderRouter {
        &self.router
    }
}

// @student w24
impl Sequencer {
    /// Week 24: a sequencer over engines that already hold state — rebuilt
    /// from their journals after a restart — whose exchange order ids
    /// continue at `next_order_id`, so an id is never assigned twice across
    /// restarts. The engines' `spec.id`s must be distinct. Orders resting
    /// in them belong to no session (their sessions died with the old
    /// process): they trade normally, and reports about them are dropped.
    /// O(engines).
    pub fn with_engines(
        engines: Vec<Engine>,
        next_order_id: OrderId,
        hooks: Hooks,
        cancel_on_disconnect: bool,
    ) -> Sequencer {
        Sequencer {
            engines: engines
                .into_iter()
                .map(|e| (e.config().spec.id, e))
                .collect(),
            router: OrderRouter::starting_at(next_order_id.0.max(1)),
            hooks,
            cancel_on_disconnect,
            events: Vec::with_capacity(256),
            sequenced: 0,
        }
    }

    /// Week 24: the engine thread found its queue empty; passes it on to
    /// [`Stage::on_idle`](crate::Stage::on_idle) (an interval fsync).
    pub fn idle(&mut self) {
        self.hooks.stage.on_idle();
    }
}

// @student-begin w13
impl Sequencer {
    fn engine_index(&self, instrument: InstrumentId) -> Option<usize> {
        self.engines.iter().position(|(id, _)| *id == instrument)
    }

    fn resolve(&self, session: SessionId, client_id: OrderId) -> Option<(OrderId, InstrumentId)> {
        let id = self.router.lookup(session, client_id)?;
        Some((id, self.router.route(id)?.instrument))
    }

    /// Sequences `cmd` (exchange ids) into `instrument`'s engine and routes
    /// the events.
    fn apply<F: FnMut(SessionId, Message)>(
        &mut self,
        instrument: InstrumentId,
        cmd: &Command,
        deliver: &mut F,
    ) {
        let Some(ix) = self.engine_index(instrument) else {
            return;
        };
        self.hooks.stage.on_command(instrument, cmd);
        self.events.clear();
        self.engines[ix].1.process(cmd, &mut self.events);
        self.sequenced += 1;
        self.hooks.stage.on_events(instrument, cmd, &self.events);
        for ev in &self.events {
            self.hooks.pretrade.on_event(ev);
        }
        self.router.route_events(cmd, &self.events, &mut |s, ev| {
            deliver(s, Message::Event(ev))
        });
    }
}
// @student-end
