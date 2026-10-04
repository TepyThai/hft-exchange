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

use super::plan::{Fate, FaultEvent, FaultPlan, Link, Scheduled};
use journal::{ToPrimary, ToStandby};
use marketdata::L2Update;

// @student-begin w48
use super::plan::{draw, Fault};
use std::collections::BTreeMap;
// @student-end

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
// @student w48
#[derive(Clone, Debug)]
pub struct Injector {
    link: Link,
    /// The faults acting on `link`, in plan order, with their streams.
    faults: Vec<(Scheduled, u64)>,
    packets: u64,
    log: Vec<FaultEvent>,
}

// @student w48
impl Injector {
    /// An injector for `link` under `plan`. O(faults).
    pub fn new(plan: &FaultPlan, link: Link) -> Injector {
        let faults = plan
            .faults
            .iter()
            .enumerate()
            .filter(|(_, s)| s.fault.acts_on(link))
            .map(|(i, s)| (*s, plan.stream(i)))
            .collect();
        Injector {
            link,
            faults,
            packets: 0,
            log: Vec::new(),
        }
    }

    /// The link it judges.
    pub fn link(&self) -> Link {
        self.link
    }

    /// The fate of the next packet, sent at `step`; logs it unless clean.
    pub fn fate(&mut self, step: u64) -> Fate {
        let n = self.packets;
        self.packets += 1;
        let mut fate = Fate::CLEAN;
        let active = || self.faults.iter().filter(|(s, _)| s.is_active(step));
        if active().any(|(s, _)| s.fault == Fault::Partition) {
            fate.partitioned = true;
        } else {
            for &(s, stream) in active() {
                let Some(per_mille) = s.fault.per_mille() else {
                    continue;
                };
                let r = draw(stream, n);
                if r % 1000 >= per_mille as u64 {
                    continue;
                }
                let param = r >> 32;
                match s.fault {
                    Fault::Drop { .. } => {
                        fate.dropped = true;
                        break;
                    }
                    Fault::Corrupt { .. } => fate.corrupt = Some(param as u32),
                    Fault::Duplicate { .. } => fate.duplicate = true,
                    Fault::Delay { max_steps, .. } => {
                        fate.delay += 1 + param % max_steps.max(1) as u64;
                    }
                    Fault::Reorder { .. } => fate.reorder = true,
                    Fault::Partition | Fault::Stall { .. } | Fault::CrashPrimary => {}
                }
            }
        }
        if !fate.is_clean() {
            self.log.push(FaultEvent::Packet {
                step,
                link: self.link,
                packet: n,
                fate,
            });
        }
        fate
    }

    /// Packets judged so far (the next packet's number).
    pub fn packets(&self) -> u64 {
        self.packets
    }

    /// Every fate that was not clean, in order.
    pub fn log(&self) -> &[FaultEvent] {
        &self.log
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
// @student w48
#[derive(Clone, Debug)]
pub struct ChaosLink<P> {
    injector: Injector,
    /// (due step, queue order) → packet.
    queue: BTreeMap<(u64, u64), P>,
    order: u64,
    /// A reordered packet waiting for the next one: (packet, copies, due).
    held: Option<(P, u8, u64)>,
}

// @student w48
impl<P: Packet> ChaosLink<P> {
    /// An empty link for `link` under `plan`.
    pub fn new(plan: &FaultPlan, link: Link) -> ChaosLink<P> {
        ChaosLink {
            injector: Injector::new(plan, link),
            queue: BTreeMap::new(),
            order: 0,
            held: None,
        }
    }

    /// Sends `packet` at `step` (see the type docs).
    pub fn send(&mut self, step: u64, packet: P) {
        let fate = self.injector.fate(step);
        if fate.is_lost() {
            return;
        }
        let mut packet = packet;
        if let Some(selector) = fate.corrupt {
            if !packet.corrupt(selector) {
                return;
            }
        }
        let copies = 1 + fate.duplicate as u8;
        let due = step + fate.delay;
        let previous = self.held.take();
        if fate.reorder {
            if let Some((p, n, d)) = previous {
                self.enqueue(d, p, n);
            }
            self.held = Some((packet, copies, due));
            return;
        }
        self.enqueue(due, packet, copies);
        if let Some((p, n, d)) = previous {
            self.enqueue(d.max(due), p, n);
        }
    }

    /// Appends every packet due by `step` to `out`, in delivery order.
    pub fn recv(&mut self, step: u64, out: &mut Vec<P>) {
        if self.held.as_ref().is_some_and(|&(_, _, due)| step > due) {
            if let Some((p, n, _)) = self.held.take() {
                self.enqueue(step, p, n);
            }
        }
        while let Some(entry) = self.queue.first_entry() {
            if entry.key().0 > step {
                break;
            }
            out.push(entry.remove());
        }
    }

    /// Packets sent but not yet received (copies counted).
    pub fn in_flight(&self) -> usize {
        self.queue.len() + self.held.as_ref().map_or(0, |h| h.1 as usize)
    }

    /// The link's injector (its packet count and fault log).
    pub fn injector(&self) -> &Injector {
        &self.injector
    }
}

// @student-begin w48
impl<P: Packet> ChaosLink<P> {
    fn enqueue(&mut self, due: u64, packet: P, copies: u8) {
        for _ in 1..copies {
            self.queue.insert((due, self.order), packet.clone());
            self.order += 1;
        }
        self.queue.insert((due, self.order), packet);
        self.order += 1;
    }
}
// @student-end
