//! Demo configuration: defaults, then `HFT_DEMO_*` environment variables,
//! then command-line flags (each layer overrides the one before), validated
//! once at startup. Bad input stops the process with a message naming the
//! setting and where it came from; nothing falls back silently.
//!
//! Provided contract for week 47: the server wiring reads a [`Config`], it
//! does not parse one.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// `demo --help`.
pub const USAGE: &str = "\
hft-exchange demo: the engine, simulated order flow and a WebSocket L2 feed

usage: demo [OPTIONS]
       demo healthcheck [OPTIONS]   GET /health on the local HTTP port; exit 0 if ready

options (each also reads an environment variable; flags win):
  --http-addr ADDR         HTTP server: page, /health, /metrics, /stats  [HFT_DEMO_HTTP_ADDR, default 0.0.0.0:8080]
  --ws-addr ADDR           WebSocket market-data feed                    [HFT_DEMO_WS_ADDR, default 0.0.0.0:8081]
  --rate N                 simulated commands per second, 0 = unthrottled [HFT_DEMO_RATE, default 2000]
  --seed N                 simulation seed (same seed, same market)      [HFT_DEMO_SEED, default 42]
  --duration D             stop after D (e.g. 30s, 1500ms, 5m)           [HFT_DEMO_DURATION, default: run until stopped]
  --shutdown-token T       enable POST /shutdown with `Authorization: Bearer T` [HFT_DEMO_SHUTDOWN_TOKEN]
  --public-ws-url URL      feed URL the page connects to (behind a proxy) [HFT_DEMO_PUBLIC_WS_URL, default ws://<page host>:<ws port>/]
  --max-ws-clients N       WebSocket connections beyond this get 503     [HFT_DEMO_MAX_WS_CLIENTS, default 256]
  --ring-capacity N        broadcast ring slots, a power of two          [HFT_DEMO_RING_CAPACITY, default 65536]
  --snapshot-interval N    feed updates between snapshots, <= ring/2     [HFT_DEMO_SNAPSHOT_INTERVAL, default 1024]
  -h, --help               this text

ADDR is HOST:PORT or just PORT (all interfaces). Port 0 picks a free port.";

/// Everything the demo needs to run. Build one with [`Config::parse`] (or
/// start from [`Config::default`] in tests and override fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Where the HTTP server listens (page, `/health`, `/metrics`, `/stats`).
    pub http_addr: SocketAddr,
    /// Where the WebSocket market-data feed listens.
    pub ws_addr: SocketAddr,
    /// Simulated commands per second; 0 runs the engine as fast as it goes.
    pub rate: u64,
    /// Seed of the simulated order flow.
    pub seed: u64,
    /// Stop by itself after this long (`None`: until asked to stop).
    pub duration: Option<Duration>,
    /// Enables `POST /shutdown` for requests carrying this bearer token.
    /// `None` disables the route (404).
    pub shutdown_token: Option<String>,
    /// The feed URL the page should connect to, when it is not
    /// `ws://<page host>:<ws port>/` (TLS-terminating proxies, other ports).
    pub public_ws_url: Option<String>,
    /// Concurrent WebSocket clients; the next one is refused with 503.
    pub max_ws_clients: usize,
    /// Slots in the broadcast ring between the engine and the feed's client
    /// threads. A power of two ≥ 2.
    pub ring_capacity: usize,
    /// Feed updates between snapshots stored for new and lagging clients;
    /// `1..=ring_capacity / 2` so a snapshot is always within the ring.
    pub snapshot_interval: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            http_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080),
            ws_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8081),
            rate: 2_000,
            seed: 42,
            duration: None,
            shutdown_token: None,
            public_ws_url: None,
            max_ws_clients: 256,
            ring_capacity: 1 << 16,
            snapshot_interval: 1_024,
        }
    }
}

/// What the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Run the demo.
    Serve(Config),
    /// `demo healthcheck`: probe a running demo's `/health` (for container
    /// health checks in images without curl).
    HealthCheck(Config),
    /// `--help`: print [`USAGE`].
    Help,
}

/// A setting that could not be used. `Display` names the setting, the bad
/// value and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A flag that does not exist.
    UnknownFlag(String),
    /// A flag given without its value.
    MissingValue(String),
    /// A value that does not parse or breaks a constraint.
    Invalid {
        /// The flag or environment variable it came from.
        source: String,
        value: String,
        reason: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::UnknownFlag(flag) => write!(f, "unknown option `{flag}` (see --help)"),
            ConfigError::MissingValue(flag) => write!(f, "option `{flag}` needs a value"),
            ConfigError::Invalid {
                source,
                value,
                reason,
            } => write!(f, "invalid {source} `{value}`: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// (flag, environment variable) for every setting, in `USAGE` order.
const SETTINGS: &[(&str, &str)] = &[
    ("--http-addr", "HFT_DEMO_HTTP_ADDR"),
    ("--ws-addr", "HFT_DEMO_WS_ADDR"),
    ("--rate", "HFT_DEMO_RATE"),
    ("--seed", "HFT_DEMO_SEED"),
    ("--duration", "HFT_DEMO_DURATION"),
    ("--shutdown-token", "HFT_DEMO_SHUTDOWN_TOKEN"),
    ("--public-ws-url", "HFT_DEMO_PUBLIC_WS_URL"),
    ("--max-ws-clients", "HFT_DEMO_MAX_WS_CLIENTS"),
    ("--ring-capacity", "HFT_DEMO_RING_CAPACITY"),
    ("--snapshot-interval", "HFT_DEMO_SNAPSHOT_INTERVAL"),
];

impl Config {
    /// Parses the arguments after the program name, reading environment
    /// variables through `env` (pass `|k| std::env::var(k).ok()`; tests
    /// pass a map). Layers: defaults, then `HFT_DEMO_*`, then flags
    /// (`--flag value` or `--flag=value`). Empty variables are ignored.
    /// O(arguments).
    pub fn parse<I, S, E>(args: I, env: E) -> Result<Invocation, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        E: Fn(&str) -> Option<String>,
    {
        let mut args: Vec<String> = args.into_iter().map(Into::into).collect();
        let health_check = args.first().is_some_and(|a| a == "healthcheck");
        if health_check {
            args.remove(0);
        }

        let mut config = Config::default();
        for &(flag, var) in SETTINGS {
            if let Some(value) = env(var).filter(|v| !v.is_empty()) {
                config.set(flag, var, &value)?;
            }
        }
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "-h" || arg == "--help" {
                return Ok(Invocation::Help);
            }
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
                _ => (arg.clone(), None),
            };
            if !SETTINGS.iter().any(|&(f, _)| f == flag) {
                return Err(ConfigError::UnknownFlag(flag));
            }
            let value = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| ConfigError::MissingValue(flag.clone()))?
                }
            };
            config.set(&flag, &flag, &value)?;
            i += 1;
        }
        config.validate()?;
        Ok(if health_check {
            Invocation::HealthCheck(config)
        } else {
            Invocation::Serve(config)
        })
    }

    /// Checks the constraints between settings ([`parse`](Self::parse)
    /// calls it; call it yourself after editing fields by hand).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |source: &str, value: String, reason| {
            Err(ConfigError::Invalid {
                source: source.to_string(),
                value,
                reason,
            })
        };
        if self.ring_capacity < 2 || !self.ring_capacity.is_power_of_two() {
            return invalid(
                "ring capacity",
                self.ring_capacity.to_string(),
                "must be a power of two >= 2",
            );
        }
        if self.snapshot_interval == 0 || self.snapshot_interval > self.ring_capacity as u64 / 2 {
            return invalid(
                "snapshot interval",
                self.snapshot_interval.to_string(),
                "must be between 1 and half the ring capacity",
            );
        }
        if self.max_ws_clients == 0 {
            return invalid("max WebSocket clients", "0".into(), "must be at least 1");
        }
        if self.shutdown_token.as_deref().is_some_and(|t| t.len() < 8) {
            return invalid(
                "shutdown token",
                "(hidden)".into(),
                "must be at least 8 characters",
            );
        }
        if let Some(url) = &self.public_ws_url {
            if !(url.starts_with("ws://") || url.starts_with("wss://")) {
                return invalid(
                    "public WebSocket URL",
                    url.clone(),
                    "must start with ws:// or wss://",
                );
            }
        }
        Ok(())
    }

    fn set(&mut self, flag: &str, source: &str, value: &str) -> Result<(), ConfigError> {
        let invalid = |reason| ConfigError::Invalid {
            source: source.to_string(),
            value: if flag == "--shutdown-token" {
                "(hidden)".into()
            } else {
                value.to_string()
            },
            reason,
        };
        let number = || {
            value
                .parse::<u64>()
                .map_err(|_| invalid("not a non-negative integer"))
        };
        match flag {
            "--http-addr" => {
                self.http_addr =
                    parse_addr(value).ok_or_else(|| invalid("not HOST:PORT or PORT"))?
            }
            "--ws-addr" => {
                self.ws_addr = parse_addr(value).ok_or_else(|| invalid("not HOST:PORT or PORT"))?
            }
            "--rate" => self.rate = number()?,
            "--seed" => self.seed = number()?,
            "--duration" => {
                self.duration = Some(
                    parse_duration(value)
                        .ok_or_else(|| invalid("not a duration like 30s, 1500ms or 5m"))?,
                )
            }
            "--shutdown-token" => self.shutdown_token = Some(value.to_string()),
            "--public-ws-url" => self.public_ws_url = Some(value.to_string()),
            "--max-ws-clients" => self.max_ws_clients = number()? as usize,
            "--ring-capacity" => self.ring_capacity = number()? as usize,
            "--snapshot-interval" => self.snapshot_interval = number()?,
            _ => return Err(ConfigError::UnknownFlag(flag.to_string())),
        }
        Ok(())
    }
}

/// `HOST:PORT` (IPv4, `[IPv6]` or `localhost`) or a bare `PORT` on all
/// interfaces.
fn parse_addr(value: &str) -> Option<SocketAddr> {
    if let Ok(port) = value.parse::<u16>() {
        return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port));
    }
    if let Some(port) = value.strip_prefix("localhost:") {
        return Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port.parse().ok()?,
        ));
    }
    value.parse().ok()
}

/// `500ms`, `30s`, `5m`, `1h`, or a bare number of seconds. Must be > 0.
fn parse_duration(value: &str) -> Option<Duration> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split);
    let n: u64 = digits.parse().ok()?;
    let d = match unit {
        "ms" => Duration::from_millis(n),
        "" | "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n.checked_mul(60)?),
        "h" => Duration::from_secs(n.checked_mul(3_600)?),
        _ => return None,
    };
    (!d.is_zero()).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(args: &[&str], env: &[(&str, &str)]) -> Result<Invocation, ConfigError> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::parse(args.iter().copied(), |k| env.get(k).cloned())
    }

    fn serve(args: &[&str], env: &[(&str, &str)]) -> Config {
        match parse(args, env) {
            Ok(Invocation::Serve(c)) => c,
            other => panic!("expected a config, got {other:?}"),
        }
    }

    #[test]
    fn defaults_then_env_then_flags() {
        assert_eq!(serve(&[], &[]), Config::default());
        let c = serve(
            &["--rate", "500", "--ws-addr=127.0.0.1:0"],
            &[
                ("HFT_DEMO_RATE", "100"),
                ("HFT_DEMO_SEED", "7"),
                ("HFT_DEMO_HTTP_ADDR", "9000"),
            ],
        );
        assert_eq!(c.rate, 500, "flag beats env");
        assert_eq!(c.seed, 7, "env beats default");
        assert_eq!(c.http_addr, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(c.ws_addr, "127.0.0.1:0".parse().unwrap());
        assert_eq!(
            serve(&["--duration", "1500ms"], &[]).duration,
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(
            serve(&["--duration", "2m"], &[]).duration,
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            serve(&[], &[("HFT_DEMO_DURATION", "3")]).duration,
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            serve(&["--http-addr", "localhost:1"], &[]).http_addr,
            "127.0.0.1:1".parse().unwrap()
        );
    }

    #[test]
    fn modes() {
        assert_eq!(parse(&["--rate", "1", "--help"], &[]), Ok(Invocation::Help));
        assert!(matches!(
            parse(&["healthcheck", "--http-addr", "8080"], &[]),
            Ok(Invocation::HealthCheck(_))
        ));
    }

    #[test]
    fn errors_name_the_setting_and_its_source() {
        let err = parse(&[], &[("HFT_DEMO_RATE", "fast")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid HFT_DEMO_RATE `fast`: not a non-negative integer"
        );
        assert_eq!(
            parse(&["--rate"], &[]),
            Err(ConfigError::MissingValue("--rate".into()))
        );
        assert_eq!(
            parse(&["--nope", "1"], &[]),
            Err(ConfigError::UnknownFlag("--nope".into()))
        );
        assert!(parse(&["--duration", "0s"], &[]).is_err());
        assert!(parse(&["--duration", "10 parsecs"], &[]).is_err());
        assert!(parse(&["--ring-capacity", "1000"], &[]).is_err());
        assert!(parse(
            &["--ring-capacity", "1024", "--snapshot-interval", "513"],
            &[]
        )
        .is_err());
        assert!(parse(&["--public-ws-url", "http://x"], &[]).is_err());
        let err = parse(&["--shutdown-token", "short"], &[]).unwrap_err();
        assert!(
            !err.to_string().contains("short"),
            "tokens are never echoed: {err}"
        );
    }
}
