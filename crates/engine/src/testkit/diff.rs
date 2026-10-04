//! Differential testing and shrinking.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[allow(unused_variables)]
pub fn differential<L: Matcher, R: Matcher>(
    left: &mut L,
    right: &mut R,
    commands: &[Command],
) -> Result<(), Box<Mismatch>> {
    todo!("w12: differential")
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
#[allow(unused_variables)]
pub fn shrink(commands: &[Command], fails: impl FnMut(&[Command]) -> bool) -> Vec<Command> {
    todo!("w12: shrink")
}
