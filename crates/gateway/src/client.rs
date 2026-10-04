//! A small blocking client for the gateway's binary protocol, used by the
//! tests, the benchmarks and the simulated traders. Provided (not part of
//! a week's task).
//!
//! ```no_run
//! use gateway::client::Client;
//! use types::{NewOrder, Side};
//!
//! let mut c = Client::connect("127.0.0.1:9000".parse().unwrap())?;
//! let ack = c.logon(7, 42)?;
//! c.send_order(&NewOrder::limit(1, Side::Buy, 100, 10))?;
//! let first = c.recv_event()?; // Accepted { id: OrderId(1) }
//! # let _ = (ack, first);
//! # Ok::<(), std::io::Error>(())
//! ```

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use types::{AccountId, Command, Event, NewOrder, OrderId, Qty};
use wire::{
    decode, encode_frame, FrameDecoder, Logon, LogonAck, Message, FRAME_HEADER_LEN, MAX_MSG_LEN,
};

/// Default read timeout: a test waiting for a report that never comes
/// fails instead of hanging.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A blocking connection speaking the week-11 protocol.
#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    decoder: FrameDecoder,
    rbuf: Box<[u8]>,
    inbox: VecDeque<Message>,
    wbuf: Vec<u8>,
    eof: bool,
}

impl Client {
    /// Connects with `TCP_NODELAY` and a [`DEFAULT_TIMEOUT`] read timeout.
    pub fn connect(addr: SocketAddr) -> io::Result<Client> {
        Client::from_stream(TcpStream::connect(addr)?)
    }

    /// Wraps a connected stream (sets `TCP_NODELAY` and the read timeout).
    pub fn from_stream(stream: TcpStream) -> io::Result<Client> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(DEFAULT_TIMEOUT))?;
        Ok(Client {
            stream,
            decoder: FrameDecoder::default(),
            rbuf: vec![0; 64 * 1024].into_boxed_slice(),
            inbox: VecDeque::new(),
            wbuf: Vec::with_capacity(4096),
            eof: false,
        })
    }

    /// The underlying socket.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// Sets the read timeout (`None`: block forever).
    pub fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Sends one message as one frame (one `write`).
    pub fn send(&mut self, msg: impl Into<Message>) -> io::Result<()> {
        let msg = msg.into();
        self.send_all(std::slice::from_ref(&msg))
    }

    /// Sends several messages with one `write`.
    pub fn send_all(&mut self, msgs: &[Message]) -> io::Result<()> {
        self.wbuf.clear();
        for m in msgs {
            let start = self.wbuf.len();
            self.wbuf.resize(start + FRAME_HEADER_LEN + MAX_MSG_LEN, 0);
            let n = encode_frame(m, &mut self.wbuf[start..]);
            self.wbuf.truncate(start + n);
        }
        self.stream.write_all(&self.wbuf)
    }

    /// Sends raw bytes (for malformed-input tests).
    pub fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes)
    }

    /// Sends `Logon` and waits for `LogonAck`. An error if the gateway
    /// closes the connection or answers anything else.
    pub fn logon(&mut self, account: u32, token: u64) -> io::Result<LogonAck> {
        self.send(Message::Logon(Logon {
            account: AccountId(account),
            token,
        }))?;
        match self.recv()? {
            Some(Message::LogonAck(ack)) => Ok(ack),
            Some(other) => Err(invalid(format!("expected LogonAck, got {other:?}"))),
            None => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "closed at logon",
            )),
        }
    }

    /// Sends a new order (client order id in `order.id`).
    pub fn send_order(&mut self, order: &NewOrder) -> io::Result<()> {
        self.send(Command::New(*order))
    }

    /// Cancels the session's order `id`.
    pub fn cancel(&mut self, id: u64) -> io::Result<()> {
        self.send(Command::Cancel { id: OrderId(id) })
    }

    /// Reduces the session's order `id` to `qty` remaining.
    pub fn reduce(&mut self, id: u64, qty: u64) -> io::Result<()> {
        self.send(Command::Reduce {
            id: OrderId(id),
            qty: Qty(qty),
        })
    }

    /// The next message from the gateway; `Ok(None)` once the gateway has
    /// closed the connection cleanly and every message has been read. A
    /// timeout is an error (`WouldBlock` / `TimedOut`).
    pub fn recv(&mut self) -> io::Result<Option<Message>> {
        loop {
            if let Some(m) = self.inbox.pop_front() {
                return Ok(Some(m));
            }
            if self.eof {
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// The next message, which must be an execution report.
    pub fn recv_event(&mut self) -> io::Result<Event> {
        match self.recv()? {
            Some(Message::Event(e)) => Ok(e),
            Some(other) => Err(invalid(format!("expected an event, got {other:?}"))),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            )),
        }
    }

    /// The next `n` execution reports.
    pub fn recv_events(&mut self, n: usize) -> io::Result<Vec<Event>> {
        (0..n).map(|_| self.recv_event()).collect()
    }

    /// Reads until the gateway closes the connection; returns every message
    /// received on the way. An error if it is reset or times out instead.
    pub fn read_to_close(&mut self) -> io::Result<Vec<Message>> {
        let mut rest = Vec::new();
        while let Some(m) = self.recv()? {
            rest.push(m);
        }
        Ok(rest)
    }

    /// Messages already received and decoded but not yet returned.
    pub fn pending(&self) -> usize {
        self.inbox.len()
    }

    /// One `read`: decodes every complete frame into the inbox.
    fn fill(&mut self) -> io::Result<()> {
        let n = match self.stream.read(&mut self.rbuf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(()),
            Err(e) => return Err(e),
        };
        if n == 0 {
            self.eof = true;
            if self.decoder.buffered() > 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "closed mid-frame",
                ));
            }
            return Ok(());
        }
        let mut input = &self.rbuf[..n];
        while let Some(frame) = self
            .decoder
            .next_frame(&mut input)
            .map_err(|e| invalid(e.to_string()))?
        {
            let msg = decode(frame).map_err(|e| invalid(e.to_string()))?;
            self.inbox.push_back(msg);
        }
        Ok(())
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
