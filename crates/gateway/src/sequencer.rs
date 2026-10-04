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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
pub struct Sequencer {
    // TODO(w13): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Sequencer {
    /// One engine per entry of `instruments` (their `spec.id`s must be
    /// distinct). Allocates the engines.
    pub fn new(
        instruments: &[EngineConfig],
        hooks: Hooks,
        cancel_on_disconnect: bool,
    ) -> Sequencer {
        todo!("w13: new")
    }

    /// A session logged on: delivers its `LogonAck` (before any report).
    pub fn open<F: FnMut(SessionId, Message)>(
        &mut self,
        session: SessionId,
        account: AccountId,
        deliver: &mut F,
    ) {
        todo!("w13: open")
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
        todo!("w13: command")
    }

    /// `session` disconnected: with cancel-on-disconnect, cancels its live
    /// orders through the engine (reports are still delivered, the shell
    /// drops them if the session is gone); tells the pre-trade hook.
    pub fn close<F: FnMut(SessionId, Message)>(&mut self, session: SessionId, deliver: &mut F) {
        todo!("w13: close")
    }

    /// End of a batch: lets the pipeline stage flush (e.g. fsync) before
    /// the batch's reports are released.
    pub fn flush(&mut self) {
        todo!("w13: flush")
    }

    /// Commands applied to the engines so far (pre-trade and gateway
    /// rejects excluded).
    pub fn sequenced(&self) -> u64 {
        todo!("w13: sequenced")
    }

    /// The engine of `instrument`.
    pub fn engine(&self, instrument: InstrumentId) -> Option<&Engine> {
        todo!("w13: engine")
    }

    /// The id map.
    pub fn router(&self) -> &OrderRouter {
        todo!("w13: router")
    }
}

#[allow(unused_variables)]
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
        todo!("w24: with_engines")
    }

    /// Week 24: the engine thread found its queue empty; passes it on to
    /// [`Stage::on_idle`](crate::Stage::on_idle) (an interval fsync).
    pub fn idle(&mut self) {
        todo!("w24: idle")
    }
}

// TODO(w13): implement — see the week guide on tepy.dev/hft.
