//! Minimal blocking clients for the demo's two ports: one-shot HTTP
//! requests and a WebSocket feed reader. Used by `demo healthcheck`, the
//! tests and the `fanout` load example. Provided infrastructure (std only).

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Connect and I/O timeout of the HTTP client.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Largest WebSocket message the client accepts.
pub const MAX_MESSAGE: usize = 16 << 20;

/// A parsed HTTP response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names as sent; look them up with [`header`](Self::header).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first header called `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as UTF-8 (lossy).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The address to dial for a server bound to `addr`: the loopback address
/// when it listens on all interfaces (`0.0.0.0` / `::`).
pub fn dial_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
        }
        _ => addr,
    }
}

/// `GET path` with `Connection: close`. One connection per call.
pub fn http_get(addr: SocketAddr, path: &str) -> io::Result<HttpResponse> {
    http_request(addr, "GET", path, &[])
}

/// One HTTP/1.1 request with extra `headers` and an empty body; reads the
/// response to the end of the connection (or `Content-Length`).
pub fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> io::Result<HttpResponse> {
    let mut stream = TcpStream::connect_timeout(&dial_addr(addr), HTTP_TIMEOUT)?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 0\r\n"
    );
    for (k, v) in headers {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut head_end = None;
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if head_end.is_none() {
            head_end = find(&raw, b"\r\n\r\n").map(|p| p + 4);
        }
        if let Some(end) = head_end {
            let head = String::from_utf8_lossy(&raw[..end]).into_owned();
            let length = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            });
            if length.is_some_and(|len| raw.len() >= end + len) {
                break;
            }
        }
    }
    let end = head_end.ok_or_else(|| bad("response ended before its headers"))?;
    let head = String::from_utf8_lossy(&raw[..end - 4]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad(&format!("bad status line `{status_line}`")))?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(HttpResponse {
        status,
        headers,
        body: raw[end..].to_vec(),
    })
}

/// A WebSocket client reading the demo's feed: text messages as strings.
pub struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsClient {
    /// Connects and performs the opening handshake. Fails with
    /// `ConnectionRefused` (message: the status line) if the server
    /// answers anything but `101`, e.g. `503` over its client limit.
    pub fn connect(addr: SocketAddr) -> io::Result<WsClient> {
        let mut stream = TcpStream::connect_timeout(&dial_addr(addr), HTTP_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let request = format!(
            "GET / HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let end = loop {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "closed during the handshake",
                ));
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(p) = find(&buf, b"\r\n\r\n") {
                break p + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let status_line = head.lines().next().unwrap_or_default().to_string();
        if !status_line.starts_with("HTTP/1.1 101") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                status_line,
            ));
        }
        let accept = marketdata::ws::accept_key(key);
        if !head.lines().any(|l| {
            l.split_once(':').is_some_and(|(k, v)| {
                k.eq_ignore_ascii_case("sec-websocket-accept") && v.trim() == accept
            })
        }) {
            return Err(bad("wrong Sec-WebSocket-Accept"));
        }
        buf.drain(..end);
        Ok(WsClient { stream, buf })
    }

    /// Sets how long [`recv`](Self::recv) waits (`None`: forever).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// The next text (or binary) message. Answers pings; a Close frame or
    /// EOF is `UnexpectedEof`; a timeout is `WouldBlock`/`TimedOut`.
    pub fn recv(&mut self) -> io::Result<String> {
        loop {
            if let Some((opcode, payload, used)) = parse_frame(&self.buf)? {
                self.buf.drain(..used);
                match opcode {
                    0x1 | 0x2 => return Ok(String::from_utf8_lossy(&payload).into_owned()),
                    0x8 => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "server closed the feed",
                        ))
                    }
                    0x9 => self.send(0xA, &payload)?,
                    _ => {}
                }
                continue;
            }
            let mut chunk = [0u8; 64 * 1024];
            let n = self.stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Sends a Close frame and drops the connection.
    pub fn close(mut self) -> io::Result<()> {
        self.send(0x8, &1000u16.to_be_bytes())
    }

    /// One masked client frame (clients must mask, RFC 6455 §5.3).
    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        debug_assert!(payload.len() <= 125, "control frames only");
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let mut frame = vec![0x80 | opcode, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        self.stream.write_all(&frame)
    }
}

/// An unmasked, unfragmented server frame at the start of `buf`:
/// (opcode, payload, bytes used), or `None` if incomplete.
fn parse_frame(buf: &[u8]) -> io::Result<Option<(u8, Vec<u8>, usize)>> {
    let [b0, b1, ..] = *buf else { return Ok(None) };
    if b0 & 0x80 == 0 || b1 & 0x80 != 0 {
        return Err(bad("fragmented or masked server frame"));
    }
    let (len, at) = match b1 & 0x7f {
        126 => match buf.get(2..4) {
            Some(b) => (u16::from_be_bytes([b[0], b[1]]) as usize, 4),
            None => return Ok(None),
        },
        127 => match buf.get(2..10) {
            Some(b) => (
                u64::from_be_bytes(b.try_into().expect("8 bytes")) as usize,
                10,
            ),
            None => return Ok(None),
        },
        n => (n as usize, 2),
    };
    if len > MAX_MESSAGE {
        return Err(bad("message too large"));
    }
    Ok(buf
        .get(at..at + len)
        .map(|p| (b0 & 0x0f, p.to_vec(), at + len)))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_parse_at_every_length_encoding() {
        for len in [0usize, 5, 125, 126, 65_535, 65_536] {
            let mut frame = vec![0x81];
            match len {
                0..=125 => frame.push(len as u8),
                126..=65_535 => {
                    frame.push(126);
                    frame.extend_from_slice(&(len as u16).to_be_bytes());
                }
                _ => {
                    frame.push(127);
                    frame.extend_from_slice(&(len as u64).to_be_bytes());
                }
            }
            frame.resize(frame.len() + len, b'x');
            assert_eq!(
                parse_frame(&frame[..frame.len() - 1]).unwrap(),
                None,
                "incomplete"
            );
            let (op, p, used) = parse_frame(&frame).unwrap().unwrap();
            assert_eq!((op, p.len(), used), (1, len, frame.len()));
        }
        assert!(parse_frame(&[0x01, 0x00]).is_err(), "fragments are refused");
    }

    #[test]
    fn unspecified_addresses_dial_loopback() {
        assert_eq!(
            dial_addr("0.0.0.0:80".parse().unwrap()),
            "127.0.0.1:80".parse().unwrap()
        );
        assert_eq!(
            dial_addr("[::]:80".parse().unwrap()),
            "[::1]:80".parse().unwrap()
        );
        assert_eq!(
            dial_addr("10.1.2.3:80".parse().unwrap()),
            "10.1.2.3:80".parse().unwrap()
        );
    }
}
