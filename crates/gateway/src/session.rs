//! Week 13: the per-connection session protocol, as a pure state machine.
//!
//! A [`Session`] sees one decoded frame at a time and answers with one
//! [`Action`] for the async shell to perform. It owns no socket, reads no
//! clock and never blocks, so a test can play a whole session's life in a
//! loop. The rules fail closed:
//!
//! | state | frame | action | next state |
//! |---|---|---|---|
//! | `AwaitingLogon` | valid `Logon`, credentials accepted | [`Action::Logon`] | `Active` |
//! | `AwaitingLogon` | `Logon`, credentials refused | `Disconnect(BadCredentials)` | `Closed` |
//! | `AwaitingLogon` | any other valid message | `Disconnect(NotLoggedOn)` | `Closed` |
//! | `Active` | `NewOrder` / `Cancel` / `Reduce` | [`Action::Forward`] | `Active` |
//! | `Active` | `Logon` | `Disconnect(DuplicateLogon)` | `Closed` |
//! | any | undecodable frame | `Disconnect(Malformed(_))` | `Closed` |
//! | `Closed` | anything | [`Action::Ignore`] | `Closed` |
//!
//! A forwarded `NewOrder` carries the session's account, whatever the
//! client wrote in the account field: a session trades for the account it
//! logged on as, and nothing else.

use std::fmt;
use types::{AccountId, Command};
#[allow(unused_imports)] // used by the implementation the learner writes
use wire::{DecodeError, FrameError, InboundView, Logon};

pub use risk::SessionId;

/// Where a [`Session`] is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// Connected; the first message must be a valid `Logon`.
    AwaitingLogon,
    /// Logged on: business messages are forwarded to the engine thread.
    Active,
    /// Finished. Every further frame is ignored.
    Closed,
}

/// Why a session ended. Reported in logs and the gateway's statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// A business message arrived before `Logon`.
    NotLoggedOn,
    /// A second `Logon` on an active session.
    DuplicateLogon,
    /// `Logon` with an account or token the gateway does not accept.
    BadCredentials,
    /// A frame whose payload is not a valid client message (unknown tag,
    /// bad length for its tag, out-of-domain field, wrong version).
    Malformed(DecodeError),
    /// A frame header that cannot be valid: the byte stream is out of sync.
    BadFrame(FrameError),
    /// No `Logon` within the gateway's logon timeout.
    LogonTimeout,
    /// The client closed its side of the connection.
    PeerClosed,
    /// A socket error.
    Io(std::io::ErrorKind),
    /// The client did not drain its execution reports and its outbound
    /// queue filled up.
    SlowConsumer,
    /// The gateway is shutting down.
    Shutdown,
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloseReason::NotLoggedOn => write!(f, "message before logon"),
            CloseReason::DuplicateLogon => write!(f, "second logon"),
            CloseReason::BadCredentials => write!(f, "bad credentials"),
            CloseReason::Malformed(e) => write!(f, "malformed message: {e}"),
            CloseReason::BadFrame(e) => write!(f, "bad frame: {e}"),
            CloseReason::LogonTimeout => write!(f, "no logon in time"),
            CloseReason::PeerClosed => write!(f, "closed by peer"),
            CloseReason::Io(k) => write!(f, "socket error: {k}"),
            CloseReason::SlowConsumer => write!(f, "slow consumer"),
            CloseReason::Shutdown => write!(f, "gateway shutdown"),
        }
    }
}

/// Which logons the gateway accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Auth {
    /// Any account with any token (tests and demos).
    #[default]
    Open,
    /// Only these `(account, token)` pairs.
    Tokens(Vec<(AccountId, u64)>),
}

impl Auth {
    /// True if `logon` presents acceptable credentials. O(accounts) for
    /// [`Auth::Tokens`]; runs once per connection, off the engine thread.
    pub fn allows(&self, logon: &Logon) -> bool {
        match self {
            Auth::Open => true,
            Auth::Tokens(list) => list
                .iter()
                .any(|&(account, token)| account == logon.account && token == logon.token),
        }
    }
}

/// What the I/O shell must do with one inbound frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Logon accepted: register the session with the engine thread, which
    /// answers with `LogonAck` before any execution report.
    Logon(Logon),
    /// Send this command to the engine thread (client order ids; a
    /// `NewOrder` already carries the session's account).
    Forward(Command),
    /// Close the connection.
    Disconnect(CloseReason),
    /// Nothing to do (the session is already closed).
    Ignore,
}

/// One connection's protocol state. Pure: frames in, [`Action`]s out.
// @student w13
#[derive(Debug)]
pub struct Session {
    id: SessionId,
    state: SessionState,
    /// Set at logon, kept after close (for logs).
    account: Option<AccountId>,
}

// @student w13
impl Session {
    /// A session for a freshly accepted connection, awaiting logon.
    pub fn new(id: SessionId) -> Session {
        Session {
            id,
            state: SessionState::AwaitingLogon,
            account: None,
        }
    }

    /// The gateway-assigned id.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Current state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The account the session logged on as (`None` before logon).
    pub fn account(&self) -> Option<AccountId> {
        self.account
    }

    /// Applies one frame payload (as returned by `wire::FrameDecoder`) and
    /// says what to do (see the module docs for the rules). O(1), no
    /// allocation: the frame is parsed with a zero-copy `InboundView`.
    pub fn on_frame(&mut self, frame: &[u8], auth: &Auth) -> Action {
        if self.state == SessionState::Closed {
            return Action::Ignore;
        }
        let view = match InboundView::parse(frame) {
            Ok(v) => v,
            Err(e) => return self.close(CloseReason::Malformed(e)),
        };
        match (self.state, view) {
            (SessionState::AwaitingLogon, InboundView::Logon(l)) => {
                let logon = l.to_logon();
                if !auth.allows(&logon) {
                    return self.close(CloseReason::BadCredentials);
                }
                self.account = Some(logon.account);
                self.state = SessionState::Active;
                Action::Logon(logon)
            }
            (SessionState::AwaitingLogon, _) => self.close(CloseReason::NotLoggedOn),
            (SessionState::Active, InboundView::Logon(_)) => {
                self.close(CloseReason::DuplicateLogon)
            }
            (SessionState::Active, InboundView::NewOrder(v)) => {
                let mut order = v.to_new_order();
                order.account = self.account.unwrap_or(order.account);
                Action::Forward(Command::New(order))
            }
            (SessionState::Active, other) => match other.to_command() {
                Some(cmd) => Action::Forward(cmd),
                None => Action::Ignore,
            },
            (SessionState::Closed, _) => Action::Ignore,
        }
    }

    /// Moves to `Closed` (if not already) and returns the matching
    /// `Disconnect` action. Idempotent.
    pub fn close(&mut self, reason: CloseReason) -> Action {
        self.state = SessionState::Closed;
        Action::Disconnect(reason)
    }
}
