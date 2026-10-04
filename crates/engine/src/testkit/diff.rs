//! Differential testing and shrinking.

use super::NaiveEngine;
use crate::Engine;
use book::{OrderBook, RestingOrder};
use types::{Command, Event, Side};

/// Anything that turns commands into events over a book: the real
/// [`Engine`] (any book), the [`NaiveEngine`], or a test double.
pub trait Matcher {
    /// Processes one command, appending its events to `out`.
    fn process(&mut self, cmd: &Command, out: &mut Vec<Event>);

    /// Clears `out` and fills it with every resting order: bids best first,
    /// then asks best first, oldest first within a price.
    fn resting(&self, out: &mut Vec<RestingOrder>);
}

impl<B: OrderBook> Matcher for Engine<B> {
    fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        Engine::process(self, cmd, out);
    }

    fn resting(&self, out: &mut Vec<RestingOrder>) {
        out.clear();
        for side in [Side::Buy, Side::Sell] {
            self.book().visit_orders(side, &mut |o| {
                out.push(o);
                true
            });
        }
    }
}

impl Matcher for NaiveEngine {
    fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        NaiveEngine::process(self, cmd, out);
    }

    fn resting(&self, out: &mut Vec<RestingOrder>) {
        NaiveEngine::resting(self, out);
    }
}

/// The first command on which two matchers disagree, in their events or in
/// the resulting book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// Index of the command in the stream.
    pub index: usize,
    pub command: Command,
    /// Events of the left / right matcher for that command.
    pub left: Vec<Event>,
    pub right: Vec<Event>,
    /// Resting orders of each matcher after that command.
    pub left_book: Vec<RestingOrder>,
    pub right_book: Vec<RestingOrder>,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "matchers diverge at command #{}: {:?}",
            self.index, self.command
        )?;
        writeln!(f, "  left events:  {:?}", self.left)?;
        writeln!(f, "  right events: {:?}", self.right)?;
        if self.left_book != self.right_book {
            writeln!(f, "  left book:  {:?}", self.left_book)?;
            writeln!(f, "  right book: {:?}", self.right_book)?;
        }
        Ok(())
    }
}

/// Feeds `commands` to both matchers and compares, after every command, the
/// events produced and the full resting book. Returns the first divergence.
/// O(commands × book size) on top of the matchers' own cost.
// @student w12
pub fn differential<L: Matcher, R: Matcher>(
    left: &mut L,
    right: &mut R,
    commands: &[Command],
) -> Result<(), Box<Mismatch>> {
    let (mut le, mut re) = (Vec::new(), Vec::new());
    let (mut lb, mut rb) = (Vec::new(), Vec::new());
    for (index, cmd) in commands.iter().enumerate() {
        le.clear();
        re.clear();
        left.process(cmd, &mut le);
        right.process(cmd, &mut re);
        left.resting(&mut lb);
        right.resting(&mut rb);
        if le != re || lb != rb {
            return Err(Box::new(Mismatch {
                index,
                command: *cmd,
                left: le,
                right: re,
                left_book: lb,
                right_book: rb,
            }));
        }
    }
    Ok(())
}

/// Minimises a failing command stream by deleting commands.
///
/// `fails` must return `true` for `commands`; the result is a subsequence
/// that still fails and is **1-minimal**: removing any single command makes
/// `fails` return `false`. Delta-debugging style: try deleting chunks of
/// half the stream, then quarters, … down to single commands, repeating
/// single-command passes until nothing more can go. If `commands` does not
/// fail, it is returned unchanged.
///
/// Cost: O(n²) calls of `fails` in the worst case, far fewer in practice.
// @student w12
pub fn shrink(commands: &[Command], fails: impl FnMut(&[Command]) -> bool) -> Vec<Command> {
    let mut fails = fails;
    let mut current = commands.to_vec();
    if !fails(&current) {
        return current;
    }
    let mut chunk = (current.len() / 2).max(1);
    let mut candidate = Vec::with_capacity(current.len());
    loop {
        let mut progress = false;
        let mut start = 0;
        while start < current.len() {
            let end = (start + chunk).min(current.len());
            candidate.clear();
            candidate.extend_from_slice(&current[..start]);
            candidate.extend_from_slice(&current[end..]);
            if fails(&candidate) {
                std::mem::swap(&mut current, &mut candidate);
                progress = true;
            } else {
                start = end;
            }
        }
        if chunk > 1 {
            chunk /= 2;
        } else if !progress {
            return current;
        }
    }
}
