//! The FIX session layer: logon, sequence numbers, heartbeats, gap recovery.
//!
//! [`Session`] is *sans-IO*: it never touches a socket or a clock. The
//! caller hands it each complete inbound message ([`Session::on_message`])
//! and calls [`Session::poll`] periodically, passing the current time in
//! nanoseconds since the Unix epoch; the session appends whatever it needs
//! to send to an output buffer, which the caller writes to the socket. The
//! same code therefore runs under Tokio, in a blocking loop, or in a test
//! with a fake clock.
//!
//! ## Rules implemented
//!
//! * **Logon first.** The acceptor waits for a Logon (anything else
//!   disconnects), checks the CompIDs, adopts the initiator's HeartBtInt and
//!   answers with a Logon. ResetSeqNumFlag (141=Y) restarts both sides at 1.
//! * **Inbound MsgSeqNum.** Equal to expected: process, expect +1. Lower
//!   without PossDupFlag: send Logout "MsgSeqNum too low, expecting X but
//!   received Y" and disconnect; lower with PossDupFlag=Y: ignore the
//!   duplicate. Higher: a gap — send one ResendRequest(expected, 0) and drop
//!   the message (the peer will resend it); a Logon, ResendRequest or Logout
//!   is still acted on.
//! * **SequenceReset.** Gap-fill mode (123=Y) is sequenced and moves the
//!   expected number to NewSeqNo; reset mode ignores MsgSeqNum and forces it.
//! * **ResendRequest from the peer** is answered with a SequenceReset
//!   gap-fill (PossDupFlag=Y) over the whole range: this session keeps no
//!   store of sent messages, so nothing is resent. (A production venue
//!   resends ExecutionReports from its journal instead.)
//! * **Heartbeats.** Send a Heartbeat when nothing was sent for HeartBtInt;
//!   send a TestRequest when nothing was received for HeartBtInt + 20%;
//!   logout and disconnect if the TestRequest is not answered within
//!   another HeartBtInt. Any inbound message counts as an answer.
//! * **Outbound MsgSeqNum** increases by one per message sent.

#[allow(unused_imports)] // used by the implementation the learner writes
use crate::encode::{format_uint, format_utc_timestamp, Encoder, StdHeader};
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::messages::{
    msg_type, FixBody, Heartbeat, Logon, Logout, ResendRequest, SequenceReset, TestRequest,
};
use crate::parse::{FixError, RawMessage};

/// Which side opens the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The client: sends the first Logon.
    Initiator,
    /// The exchange: waits for a Logon and answers it.
    Acceptor,
}

/// Static session parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// Initiator or acceptor.
    pub role: Role,
    /// Our CompID (49 on what we send, 56 on what we receive).
    pub sender_comp_id: String,
    /// The peer's CompID.
    pub target_comp_id: String,
    /// HeartBtInt (108) an initiator proposes, in seconds; an acceptor uses
    /// the initiator's value instead. 0 disables heartbeats.
    pub heartbeat_interval_s: u32,
    /// Initiator: ask for ResetSeqNumFlag (141=Y) in the Logon.
    pub reset_seq_num: bool,
    /// How long to wait for the Logon / Logout handshake, in seconds.
    pub handshake_timeout_s: u32,
}

impl SessionConfig {
    /// A config with HeartBtInt 30 s, no sequence reset, 10 s handshake
    /// timeout.
    pub fn new(role: Role, sender_comp_id: &str, target_comp_id: &str) -> SessionConfig {
        SessionConfig {
            role,
            sender_comp_id: sender_comp_id.to_string(),
            target_comp_id: target_comp_id.to_string(),
            heartbeat_interval_s: 30,
            reset_seq_num: false,
            handshake_timeout_s: 10,
        }
    }
}

/// Where the session is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// Created; `start` not called yet.
    Idle,
    /// Acceptor: connected, waiting for the peer's Logon.
    AwaitingLogon,
    /// Initiator: Logon sent, waiting for the answer.
    LogonSent,
    /// Logged on: application messages flow.
    Active,
    /// We sent Logout and wait for the peer's.
    LogoutSent,
    /// Over: close the connection.
    Disconnected,
}

/// Why the session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisconnectReason {
    /// We sent Logout and the peer confirmed.
    LogoutComplete,
    /// The peer sent Logout and we confirmed.
    PeerLogout,
    /// The first message was not a Logon.
    NotLoggedOn,
    /// The Logon was missing required fields or had invalid values.
    BadLogon,
    /// SenderCompID / TargetCompID do not match the configuration.
    CompIdMismatch,
    /// MsgSeqNum lower than expected without PossDupFlag.
    SeqTooLow {
        /// The MsgSeqNum we expected.
        expected: u64,
        /// The one we got.
        received: u64,
    },
    /// A second Logon while already logged on.
    UnexpectedLogon,
    /// A TestRequest went unanswered.
    HeartbeatTimeout,
    /// No Logon answer within the handshake timeout.
    LogonTimeout,
    /// No Logout answer within the handshake timeout.
    LogoutTimeout,
}

/// Why an inbound message was dropped without reaching the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dropped {
    /// Failed validation (bad checksum, body length, syntax): ignored, the
    /// expected MsgSeqNum is unchanged.
    Garbled(FixError),
    /// PossDupFlag=Y and already seen.
    Duplicate {
        /// Its MsgSeqNum.
        seq: u64,
    },
    /// Ahead of the expected MsgSeqNum: a ResendRequest is out.
    Gap {
        /// The MsgSeqNum we expected.
        expected: u64,
        /// The one we got.
        received: u64,
    },
}

/// What happened to one inbound message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received<'a> {
    /// An in-sequence application message: hand it to the application.
    App(RawMessage<'a>),
    /// A session-level message, fully handled.
    Admin,
    /// Not processed (see [`Dropped`]).
    Dropped(Dropped),
    /// The session is over: write out what was queued, then close.
    Disconnect(DisconnectReason),
}

// @student w14
/// One FIX session (one TCP connection's worth of protocol state).
#[derive(Debug)]
pub struct Session {
    cfg: SessionConfig,
    state: SessionState,
    ended: Option<DisconnectReason>,
    /// Next MsgSeqNum we expect from the peer.
    next_in: u64,
    /// MsgSeqNum of the next message we send.
    next_out: u64,
    /// Heartbeat interval in nanoseconds (0 = off).
    hb_ns: u64,
    hb_s: u32,
    last_rx: u64,
    last_tx: u64,
    /// When the state last changed (handshake timeouts).
    since: u64,
    /// When our outstanding TestRequest was sent.
    test_req_sent: Option<u64>,
    test_req_count: u64,
    /// A ResendRequest is out; the gap closes when `next_in` passes this.
    resend_until: Option<u64>,
    enc: Encoder,
}

// @student w14
impl Session {
    /// A session in [`SessionState::Idle`] with both sequence numbers at 1.
    pub fn new(cfg: SessionConfig) -> Session {
        let hb_s = cfg.heartbeat_interval_s;
        Session {
            cfg,
            state: SessionState::Idle,
            ended: None,
            next_in: 1,
            next_out: 1,
            hb_ns: 0,
            hb_s,
            last_rx: 0,
            last_tx: 0,
            since: 0,
            test_req_sent: None,
            test_req_count: 0,
            resend_until: None,
            enc: Encoder::new(),
        }
    }

    /// The transport is connected at `now`. An initiator sends its Logon
    /// (resetting both sequence numbers first if configured); an acceptor
    /// starts waiting for one.
    pub fn start(&mut self, now: u64, out: &mut Vec<u8>) {
        self.since = now;
        self.last_rx = now;
        self.last_tx = now;
        match self.cfg.role {
            Role::Acceptor => self.state = SessionState::AwaitingLogon,
            Role::Initiator => {
                if self.cfg.reset_seq_num {
                    self.next_in = 1;
                    self.next_out = 1;
                }
                let logon = Logon {
                    heart_bt_int: self.cfg.heartbeat_interval_s,
                    reset_seq_num: self.cfg.reset_seq_num,
                    username: None,
                    password: None,
                };
                self.send_next(&logon, now, out);
                self.state = SessionState::LogonSent;
            }
        }
    }

    /// Processes one complete inbound message (as delimited by
    /// [`crate::message_len`]) received at `now`, appending any replies to
    /// `out`. O(message length); no allocation in steady state.
    pub fn on_message<'a>(&mut self, msg: &'a [u8], now: u64, out: &mut Vec<u8>) -> Received<'a> {
        if let Some(reason) = self.ended {
            return Received::Disconnect(reason);
        }
        let raw = match RawMessage::parse(msg) {
            Ok(raw) => raw,
            Err(e) => return Received::Dropped(Dropped::Garbled(e)),
        };
        let logged_on = matches!(self.state, SessionState::Active | SessionState::LogoutSent);
        if raw.sender != self.cfg.target_comp_id.as_bytes()
            || raw.target != self.cfg.sender_comp_id.as_bytes()
        {
            return self.end(
                DisconnectReason::CompIdMismatch,
                logged_on.then_some(b"CompID problem"),
                now,
                out,
            );
        }
        self.last_rx = now;
        self.test_req_sent = None;

        let mt = raw.msg_type;
        let is_logon = mt == msg_type::LOGON;
        if !logged_on && !is_logon {
            return self.end(DisconnectReason::NotLoggedOn, None, now, out);
        }
        if logged_on && is_logon {
            return self.end(
                DisconnectReason::UnexpectedLogon,
                Some(b"Unexpected Logon"),
                now,
                out,
            );
        }
        let logon = if is_logon {
            match Logon::decode(&raw) {
                Ok(l) => Some(l),
                Err(_) => {
                    return self.end(DisconnectReason::BadLogon, Some(b"Invalid Logon"), now, out)
                }
            }
        } else {
            None
        };
        if logon.is_some_and(|l| l.reset_seq_num) {
            self.next_in = 1;
            if self.cfg.role == Role::Acceptor {
                self.next_out = 1;
            }
        }

        // Reset mode ignores MsgSeqNum entirely.
        if mt == msg_type::SEQUENCE_RESET {
            match SequenceReset::decode(&raw) {
                Ok(sr) if !sr.gap_fill => {
                    self.next_in = self.next_in.max(sr.new_seq_no);
                    self.close_gap();
                    return Received::Admin;
                }
                Ok(_) => {}
                Err(e) => return Received::Dropped(Dropped::Garbled(e)),
            }
        }

        let (seq, expected) = (raw.seq_num, self.next_in);
        if seq < expected {
            if raw.poss_dup {
                return Received::Dropped(Dropped::Duplicate { seq });
            }
            let mut text = [0u8; 80];
            let text = seq_too_low_text(expected, seq, &mut text);
            return self.end(
                DisconnectReason::SeqTooLow {
                    expected,
                    received: seq,
                },
                Some(text),
                now,
                out,
            );
        }

        if let Some(l) = logon {
            self.on_logon(&l, now, out);
        }

        if seq > expected {
            if self.resend_until.is_none() {
                self.send_next(
                    &ResendRequest {
                        begin_seq_no: expected,
                        end_seq_no: 0,
                    },
                    now,
                    out,
                );
            }
            self.resend_until = Some(self.resend_until.unwrap_or(0).max(seq));
            if mt == msg_type::RESEND_REQUEST {
                self.on_resend_request(&raw, now, out);
            } else if mt == msg_type::LOGOUT {
                return self.on_logout(now, out);
            }
            return if is_logon {
                Received::Admin
            } else {
                Received::Dropped(Dropped::Gap {
                    expected,
                    received: seq,
                })
            };
        }

        // In sequence.
        self.next_in = seq + 1;
        let result = match mt {
            msg_type::LOGON | msg_type::HEARTBEAT | msg_type::REJECT => Received::Admin,
            msg_type::TEST_REQUEST => {
                if let Ok(tr) = TestRequest::decode(&raw) {
                    self.send_next(
                        &Heartbeat {
                            test_req_id: Some(tr.test_req_id),
                        },
                        now,
                        out,
                    );
                }
                Received::Admin
            }
            msg_type::RESEND_REQUEST => {
                self.on_resend_request(&raw, now, out);
                Received::Admin
            }
            msg_type::SEQUENCE_RESET => {
                if let Ok(sr) = SequenceReset::decode(&raw) {
                    self.next_in = self.next_in.max(sr.new_seq_no);
                }
                Received::Admin
            }
            msg_type::LOGOUT => return self.on_logout(now, out),
            _ => Received::App(raw),
        };
        self.close_gap();
        result
    }

    /// Timers: heartbeats, TestRequests, handshake timeouts. Call it
    /// regularly (e.g. every 100 ms, or whenever the socket is idle) with
    /// the current time. Returns why the session ended, if it did.
    pub fn poll(&mut self, now: u64, out: &mut Vec<u8>) -> Option<DisconnectReason> {
        let timeout = self.cfg.handshake_timeout_s as u64 * NANOS_PER_SEC;
        match self.state {
            SessionState::Idle => None,
            SessionState::Disconnected => self.ended,
            SessionState::AwaitingLogon | SessionState::LogonSent => {
                (now.saturating_sub(self.since) >= timeout)
                    .then(|| self.end_reason(DisconnectReason::LogonTimeout, None, now, out))
            }
            SessionState::LogoutSent => (now.saturating_sub(self.since) >= timeout)
                .then(|| self.end_reason(DisconnectReason::LogoutTimeout, None, now, out)),
            SessionState::Active => {
                if self.hb_ns == 0 {
                    return None;
                }
                match self.test_req_sent {
                    Some(sent) if now.saturating_sub(sent) >= self.hb_ns => {
                        return Some(self.end_reason(
                            DisconnectReason::HeartbeatTimeout,
                            Some(b"Heartbeat timeout"),
                            now,
                            out,
                        ));
                    }
                    Some(_) => {}
                    None if now.saturating_sub(self.last_rx) >= self.hb_ns + self.hb_ns / 5 => {
                        self.test_req_count += 1;
                        let mut id = [0u8; 20];
                        let id = format_uint(self.test_req_count, &mut id);
                        self.send_next(&TestRequest { test_req_id: id }, now, out);
                        self.test_req_sent = Some(now);
                    }
                    None => {}
                }
                if now.saturating_sub(self.last_tx) >= self.hb_ns {
                    self.send_next(&Heartbeat { test_req_id: None }, now, out);
                }
                None
            }
        }
    }

    /// Sends an application message (e.g. an ExecutionReport) with the next
    /// MsgSeqNum and returns that number; `None` (nothing sent) unless the
    /// session is [`SessionState::Active`].
    pub fn send<M: FixBody>(&mut self, msg: &M, now: u64, out: &mut Vec<u8>) -> Option<u64> {
        (self.state == SessionState::Active).then(|| self.send_next(msg, now, out))
    }

    /// Starts a graceful logout: sends Logout and waits for the peer's.
    pub fn logout(&mut self, text: Option<&[u8]>, now: u64, out: &mut Vec<u8>) {
        if self.state == SessionState::Active {
            self.send_next(&Logout { text }, now, out);
            self.state = SessionState::LogoutSent;
            self.since = now;
        }
    }

    /// Current state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The MsgSeqNum expected on the next inbound message.
    pub fn next_inbound_seq(&self) -> u64 {
        self.next_in
    }

    /// The MsgSeqNum the next outbound message will carry.
    pub fn next_outbound_seq(&self) -> u64 {
        self.next_out
    }

    /// Negotiated HeartBtInt in seconds (0 = heartbeats off).
    pub fn heartbeat_interval_s(&self) -> u32 {
        self.hb_s
    }

    /// Why the session ended, once it has.
    pub fn disconnect_reason(&self) -> Option<DisconnectReason> {
        self.ended
    }
}

// @student-begin w14
const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Session internals: message handlers and sending.
impl Session {
    fn on_logon(&mut self, logon: &Logon<'_>, now: u64, out: &mut Vec<u8>) {
        match self.cfg.role {
            Role::Acceptor => {
                self.hb_s = logon.heart_bt_int;
                let reply = Logon {
                    heart_bt_int: logon.heart_bt_int,
                    reset_seq_num: logon.reset_seq_num,
                    username: None,
                    password: None,
                };
                self.send_next(&reply, now, out);
            }
            Role::Initiator => self.hb_s = logon.heart_bt_int,
        }
        self.hb_ns = self.hb_s as u64 * NANOS_PER_SEC;
        self.state = SessionState::Active;
        self.since = now;
    }

    /// Answers a ResendRequest with one gap fill over the whole range.
    fn on_resend_request(&mut self, raw: &RawMessage<'_>, now: u64, out: &mut Vec<u8>) {
        let Ok(rr) = ResendRequest::decode(raw) else {
            return;
        };
        if rr.begin_seq_no == 0 || rr.begin_seq_no >= self.next_out {
            return;
        }
        let fill = SequenceReset {
            new_seq_no: self.next_out,
            gap_fill: true,
        };
        self.send_with_seq(&fill, rr.begin_seq_no, true, now, out);
    }

    fn on_logout<'a>(&mut self, now: u64, out: &mut Vec<u8>) -> Received<'a> {
        if self.state == SessionState::LogoutSent {
            self.state = SessionState::Disconnected;
            self.ended = Some(DisconnectReason::LogoutComplete);
            Received::Disconnect(DisconnectReason::LogoutComplete)
        } else {
            self.send_next(&Logout { text: None }, now, out);
            self.state = SessionState::Disconnected;
            self.ended = Some(DisconnectReason::PeerLogout);
            Received::Disconnect(DisconnectReason::PeerLogout)
        }
    }

    fn close_gap(&mut self) {
        if self.resend_until.is_some_and(|until| self.next_in > until) {
            self.resend_until = None;
        }
    }

    /// Ends the session, sending a Logout with `text` first if given.
    fn end<'a>(
        &mut self,
        reason: DisconnectReason,
        text: Option<&[u8]>,
        now: u64,
        out: &mut Vec<u8>,
    ) -> Received<'a> {
        Received::Disconnect(self.end_reason(reason, text, now, out))
    }

    fn end_reason(
        &mut self,
        reason: DisconnectReason,
        text: Option<&[u8]>,
        now: u64,
        out: &mut Vec<u8>,
    ) -> DisconnectReason {
        if let Some(text) = text {
            self.send_next(&Logout { text: Some(text) }, now, out);
        }
        self.state = SessionState::Disconnected;
        self.ended = Some(reason);
        reason
    }

    fn send_next<M: FixBody>(&mut self, msg: &M, now: u64, out: &mut Vec<u8>) -> u64 {
        let seq = self.next_out;
        self.next_out += 1;
        self.send_with_seq(msg, seq, false, now, out);
        seq
    }

    fn send_with_seq<M: FixBody>(
        &mut self,
        msg: &M,
        seq: u64,
        poss_dup: bool,
        now: u64,
        out: &mut Vec<u8>,
    ) {
        let mut ts = [0u8; 21];
        format_utc_timestamp(now, &mut ts);
        let header = StdHeader {
            sender: self.cfg.sender_comp_id.as_bytes(),
            target: self.cfg.target_comp_id.as_bytes(),
            seq_num: seq,
            sending_time: &ts,
            poss_dup,
        };
        self.enc.encode(&header, msg, out);
        self.last_tx = now;
    }
}

/// "MsgSeqNum too low, expecting X but received Y" in a stack buffer.
fn seq_too_low_text(expected: u64, received: u64, buf: &mut [u8; 80]) -> &[u8] {
    use std::io::Write;
    let mut cur = std::io::Cursor::new(&mut buf[..]);
    let _ = write!(
        cur,
        "MsgSeqNum too low, expecting {expected} but received {received}"
    );
    let n = cur.position() as usize;
    &buf[..n]
}
// @student-end
