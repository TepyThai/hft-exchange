//! Shared helpers for the demo's week tests: a loopback config, polling,
//! a small JSON reader, and a feed book.

#![allow(dead_code)]

use demo::Config;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Both servers on loopback, ephemeral ports.
pub fn local_config(rate: u64, seed: u64) -> Config {
    Config {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        rate,
        seed,
        ..Config::default()
    }
}

/// Polls `f` every 5 ms until it returns `Some`, or panics after `timeout`.
pub fn wait_for<T>(timeout: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A parsed JSON value (numbers as f64; the demo's integers stay exact
/// below 2^53).
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Parses a whole document; panics with the input on malformed JSON.
    pub fn parse(text: &str) -> Json {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        let v = p.value();
        p.ws();
        assert_eq!(p.i, p.b.len(), "trailing data in JSON: {text}");
        v
    }

    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(fields) => fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("no key `{key}` in {self:?}")),
            _ => panic!("not an object: {self:?}"),
        }
    }

    pub fn has(&self, key: &str) -> bool {
        matches!(self, Json::Obj(f) if f.iter().any(|(k, _)| k == key))
    }

    pub fn u64(&self) -> u64 {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => *n as u64,
            _ => panic!("not a non-negative integer: {self:?}"),
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => panic!("not an array: {self:?}"),
        }
    }

    pub fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("not a string: {self:?}"),
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) {
        self.ws();
        assert_eq!(
            self.b.get(self.i),
            Some(&c),
            "expected `{}` at {}",
            c as char,
            self.i
        );
        self.i += 1;
    }

    fn value(&mut self) -> Json {
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Json::Obj(fields);
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.value() else {
                        panic!("object key must be a string")
                    };
                    self.eat(b':');
                    fields.push((k, self.value()));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Json::Obj(fields);
                        }
                        other => panic!("bad object at {}: {other:?}", self.i),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Json::Arr(items);
                }
                loop {
                    items.push(self.value());
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Json::Arr(items);
                        }
                        other => panic!("bad array at {}: {other:?}", self.i),
                    }
                }
            }
            Some(b'"') => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    match self.b[self.i] {
                        b'"' => {
                            self.i += 1;
                            return Json::Str(s);
                        }
                        b'\\' => {
                            let c = self.b[self.i + 1];
                            self.i += 2;
                            match c {
                                b'n' => s.push('\n'),
                                b't' => s.push('\t'),
                                b'u' => {
                                    let hex =
                                        std::str::from_utf8(&self.b[self.i..self.i + 4]).unwrap();
                                    s.push(
                                        char::from_u32(u32::from_str_radix(hex, 16).unwrap())
                                            .unwrap_or('?'),
                                    );
                                    self.i += 4;
                                }
                                c => s.push(c as char),
                            }
                        }
                        _ => {
                            let start = self.i;
                            while !matches!(self.b[self.i], b'"' | b'\\') {
                                self.i += 1;
                            }
                            s.push_str(std::str::from_utf8(&self.b[start..self.i]).unwrap());
                        }
                    }
                }
            }
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'n') => self.word("null", Json::Null),
            _ => {
                let start = self.i;
                while self
                    .b
                    .get(self.i)
                    .is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
                Json::Num(
                    text.parse()
                        .unwrap_or_else(|_| panic!("bad number `{text}` at {start}")),
                )
            }
        }
    }

    fn word(&mut self, w: &str, v: Json) -> Json {
        assert!(
            self.b[self.i..].starts_with(w.as_bytes()),
            "bad literal at {}",
            self.i
        );
        self.i += w.len();
        v
    }
}

/// An L2 book kept from the feed's JSON messages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedBook {
    pub seq: u64,
    /// price → qty
    pub bids: BTreeMap<i64, u64>,
    pub asks: BTreeMap<i64, u64>,
}

impl FeedBook {
    /// Applies a `snapshot` or `updates` message; returns its type.
    pub fn apply(&mut self, msg: &Json) -> String {
        let kind = msg.get("type").str().to_string();
        let seq = msg.get("seq").u64();
        match kind.as_str() {
            "snapshot" => {
                let levels = |side: &Json| -> BTreeMap<i64, u64> {
                    side.arr()
                        .iter()
                        .map(|l| (l.arr()[0].u64() as i64, l.arr()[1].u64()))
                        .collect()
                };
                self.bids = levels(msg.get("bids"));
                self.asks = levels(msg.get("asks"));
            }
            "updates" => {
                assert!(seq > self.seq, "batch seq {seq} after {}", self.seq);
                for u in msg.get("updates").arr() {
                    let useq = u.get("seq").u64();
                    assert!(
                        useq > self.seq && useq <= seq,
                        "update {useq} outside batch ({}, {seq}]",
                        self.seq
                    );
                    let side = match u.get("side").str() {
                        "buy" => &mut self.bids,
                        "sell" => &mut self.asks,
                        other => panic!("bad side {other}"),
                    };
                    let (price, qty) = (u.get("price").u64() as i64, u.get("qty").u64());
                    if qty == 0 {
                        side.remove(&price);
                    } else {
                        side.insert(price, qty);
                    }
                }
            }
            other => panic!("unknown message type {other}"),
        }
        self.seq = seq;
        kind
    }

    pub fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }
}
