//! The injector: a plan's packet faults applied at one link — the seam
//! between a sender and a receiver that the simulation owns.
//!
//! * [`Injector`] judges packets: one [`Fate`] per packet, by the rules in
//!   [`Fate`]'s docs, logged when it is not clean. Scenarios whose wire is
//!   a stream (an order session) use it directly.
//! * [`ChaosLink`] is a datagram link built on it: packets go in with
//!   [`send`](ChaosLink::send) at a step and come out of
//!   [`recv`](ChaosLink::recv) at the step they are due — lost, damaged,
//!   doubled, late or out of order as the plan says.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::plan::{Fate, FaultEvent, FaultPlan, Link, Scheduled};
use journal::{ToPrimary, ToStandby};
use marketdata::L2Update;

// TODO(w48): implement — see the week guide on tepy.dev/hft.

/// Something a [`ChaosLink`] can carry.
pub trait Packet: Clone {
    /// Damages the packet in transit: flips bit `selector % bits` of its
    /// bytes and returns `true`. Returns `false` if the packet has no
    /// checksummed payload the receiver would verify — its NIC (or the
    /// transport) would discard the damaged packet, so the link loses it
    /// instead.
    fn corrupt(&mut self, selector: u32) -> bool;
}

impl Packet for Vec<u8> {
    /// Flips one bit; an empty datagram cannot be damaged (it is lost).
    fn corrupt(&mut self, selector: u32) -> bool {
        flip_bit(self, selector)
    }
}

impl Packet for ToStandby {
    /// Damages a journal frame (the standby's CRC check must catch it); a
    /// damaged heartbeat is discarded.
    fn corrupt(&mut self, selector: u32) -> bool {
        match self {
            ToStandby::Frame(frame) => flip_bit(frame.as_bytes_mut(), selector),
            ToStandby::Heartbeat { .. } => false,
        }
    }
}

impl Packet for ToPrimary {
    /// Acknowledgements carry no checksummed payload here: lost instead.
    fn corrupt(&mut self, _selector: u32) -> bool {
        false
    }
}

impl Packet for L2Update {
    /// An update travels in a checksummed datagram: lost instead.
    fn corrupt(&mut self, _selector: u32) -> bool {
        false
    }
}

impl Packet for u64 {
    /// Test payloads: lost instead.
    fn corrupt(&mut self, _selector: u32) -> bool {
        false
    }
}

fn flip_bit(bytes: &mut [u8], selector: u32) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let bit = selector as usize % (bytes.len() * 8);
    bytes[bit / 8] ^= 1 << (bit % 8);
    true
}

/// Judges the packets of one link against a [`FaultPlan`]: packet `n`
/// (counting from 0, every packet judged) gets the [`Fate`] the plan's
/// faults on this link give it at the step it is sent — exactly the rules
/// in [`Fate`]'s docs — and every fate that is not clean is logged.
///
/// Cost per packet: O(faults on the link), no allocation except the log.
#[derive(Clone, Debug)]
pub struct Injector {
    // TODO(w48): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Injector {
    /// An injector for `link` under `plan`. O(faults).
    pub fn new(plan: &FaultPlan, link: Link) -> Injector {
        todo!("w48: new")
    }

    /// The link it judges.
    pub fn link(&self) -> Link {
        todo!("w48: link")
    }

    /// The fate of the next packet, sent at `step`; logs it unless clean.
    pub fn fate(&mut self, step: u64) -> Fate {
        todo!("w48: fate")
    }

    /// Packets judged so far (the next packet's number).
    pub fn packets(&self) -> u64 {
        todo!("w48: packets")
    }

    /// Every fate that was not clean, in order.
    pub fn log(&self) -> &[FaultEvent] {
        todo!("w48: log")
    }
}

/// A simulated datagram link (UDP-like) with a plan's faults injected.
///
/// [`send`](ChaosLink::send) judges a packet with the link's [`Injector`]:
///
/// * lost → never delivered;
/// * corrupt → [`Packet::corrupt`] damages it (or, if it cannot be
///   damaged, it is lost);
/// * otherwise it is due at `step + delay`, twice (adjacent) if
///   duplicated;
/// * reordered → held back and delivered right after the next packet sent
///   on the link (no earlier than its own due step), or one step after its
///   due step if no packet follows by then. A packet held while another is
///   held releases the first.
///
/// [`recv`](ChaosLink::recv) at `step` returns every packet due by `step`,
/// ordered by due step, then by the order they were queued. Without faults
/// a packet sent at a step is received at that step.
///
/// Cost: O(log in-flight) per packet; allocates per packet in flight.
#[derive(Clone, Debug)]
pub struct ChaosLink<P> {
    // TODO(w48): design this type's fields.
    _todo: std::marker::PhantomData<(P,)>,
}

#[allow(unused_variables)]
impl<P: Packet> ChaosLink<P> {
    /// An empty link for `link` under `plan`.
    pub fn new(plan: &FaultPlan, link: Link) -> ChaosLink<P> {
        todo!("w48: new")
    }

    /// Sends `packet` at `step` (see the type docs).
    pub fn send(&mut self, step: u64, packet: P) {
        todo!("w48: send")
    }

    /// Appends every packet due by `step` to `out`, in delivery order.
    pub fn recv(&mut self, step: u64, out: &mut Vec<P>) {
        todo!("w48: recv")
    }

    /// Packets sent but not yet received (copies counted).
    pub fn in_flight(&self) -> usize {
        todo!("w48: in_flight")
    }

    /// The link's injector (its packet count and fault log).
    pub fn injector(&self) -> &Injector {
        todo!("w48: injector")
    }
}

// TODO(w48): implement — see the week guide on tepy.dev/hft.
