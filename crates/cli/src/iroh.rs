//! `iroh://` remotes and `rgit serve --iroh`.
//!
//! Everything Iroh-specific in the CLI lives here. Without the `iroh` cargo feature the types
//! below still exist (so command lines and JSON stay the same), but every Iroh operation
//! answers with a clear "this build does not include Iroh" error.
//!
//! Configuration, highest priority first: `serve` flags, environment, `iroh.conf` in the rgit
//! config directory (`RGIT_HOME`, else the platform location). See docs/relay.md.
//!
//! | key (file) / variable                  | meaning |
//! |---|---|
//! | `relay` / `RGIT_IROH_RELAY` (comma list) | relay URL(s) to use; none = direct addresses only |
//! | `public_relay` / `RGIT_IROH_PUBLIC_RELAY` | opt in to n0's public relays (development only) |
//! | `relay_ca` / `RGIT_IROH_RELAY_CA`      | PEM file with the CA(s) that signed a private relay's certificate |
//! | `relay_token` / `RGIT_IROH_RELAY_TOKEN` | bearer token for a relay with `access.shared_token` (a secret) |
//! | `direct` / `RGIT_IROH_DIRECT`          | `false` = never use direct UDP paths (relay only) |
//! | `bind` / `RGIT_IROH_BIND`              | bind the UDP socket to this `ip:port` |
//! | `key_file` / `RGIT_IROH_KEY_FILE`      | persistent endpoint key (clients are ephemeral by default) |
//! | `connect_timeout` / `RGIT_IROH_CONNECT_TIMEOUT` | seconds, default 20 |

use std::path::PathBuf;

use clap::Args;
use rustgit_core::Error;
use rustgit_protocol::Transport;
use serde_json::Value;

/// Flags of `rgit serve` that belong to the Iroh transport.
#[derive(Args, Debug, Clone, Default)]
pub struct ServeArgs {
    /// Also accept connections over Iroh (QUIC with NAT traversal and relay fallback) and print
    /// the iroh:// URL to hand out (use it as `rgit invite create --hint URL`). Needs device
    /// authentication: refused together with --insecure-open-access
    #[arg(long)]
    pub iroh: bool,
    /// Relay URL for --iroh (repeatable; also RGIT_IROH_RELAY or `relay` in iroh.conf)
    #[arg(long = "iroh-relay", value_name = "URL")]
    pub iroh_relay: Vec<String>,
    /// Where the host's Iroh endpoint key lives (created on first use).
    /// Default: <hosted dir>/.rgit/host/iroh.key. The public half is the endpoint id in the URL
    #[arg(long, value_name = "FILE")]
    pub iroh_key_file: Option<PathBuf>,
    /// Include this machine's IP addresses in the printed iroh:// URL (faster LAN connection,
    /// but whoever gets the URL learns your private addresses)
    #[arg(long)]
    pub iroh_hint_addrs: bool,
}

/// `{"transport":"iroh","state":"direct|relayed|unreachable|connecting",...}` for the iroh
/// connection used by this command, if there was one.
pub fn connection_json() -> Option<Value> {
    imp::connection_json()
}

/// Remote URL -> transport for `iroh://`.
pub fn transport(url: &str) -> Result<Box<dyn Transport>, String> {
    imp::transport(url)
}

/// Adds an Iroh listener next to the TCP one when `--iroh` is given.
pub fn extend_listener(
    tcp: Box<dyn rustgit_protocol::Listener>,
    args: &ServeArgs,
    hosted_dir: &std::path::Path,
    open_access: bool,
) -> Result<Box<dyn rustgit_protocol::Listener>, Error> {
    imp::extend_listener(tcp, args, hosted_dir, open_access)
}

#[cfg(not(feature = "iroh"))]
mod imp {
    use super::*;

    const MISSING: &str = "this build of rgit does not include the Iroh transport; \
                           rebuild with `cargo build --release --features iroh` (see docs/relay.md)";

    pub fn connection_json() -> Option<Value> {
        None
    }
    pub fn transport(_url: &str) -> Result<Box<dyn Transport>, String> {
        Err(MISSING.into())
    }
    pub fn extend_listener(
        tcp: Box<dyn rustgit_protocol::Listener>,
        args: &ServeArgs,
        _dir: &std::path::Path,
        _open: bool,
    ) -> Result<Box<dyn rustgit_protocol::Listener>, Error> {
        if args.iroh {
            return Err(Error::Other(MISSING.into()));
        }
        Ok(tcp)
    }
}

#[cfg(feature = "iroh")]
mod imp {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    use rustgit_transport_iroh::{
        load_ca_pem, load_or_create_secret_key, FanInListener, IrohConfig, IrohListener,
        IrohTransport, IrohUrl, RelaySetting, Status, StatusHandle,
    };
    use serde_json::json;

    use super::*;
    use crate::out;

    static CURRENT: Mutex<Option<StatusHandle>> = Mutex::new(None);

    fn status_json(s: &Status) -> Value {
        json!({
            "transport": "iroh",
            "state": s.state.name(),
            "path": s.selected,
            "used_relay": s.used_relay,
            "used_direct": s.used_direct,
            "path_switches": s.switches,
            "error": s.error,
            "remote": s.remote,
            "local": s.local,
        })
    }

    pub fn connection_json() -> Option<Value> {
        CURRENT
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| status_json(&h.snapshot()))
    }

    // ---- configuration -----------------------------------------------------------------------

    /// `key = value` lines, `#` comments; `relay` may repeat.
    fn read_file(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(m),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                return Err(format!(
                    "{}:{}: expected `key = value`",
                    path.display(),
                    n + 1
                ));
            };
            let k = k.trim();
            if ![
                "relay",
                "public_relay",
                "relay_ca",
                "relay_token",
                "direct",
                "bind",
                "key_file",
                "connect_timeout",
            ]
            .contains(&k)
            {
                return Err(format!("{}:{}: unknown key '{k}'", path.display(), n + 1));
            }
            m.entry(k.to_string())
                .or_default()
                .push(v.trim().to_string());
        }
        Ok(m)
    }

    fn truthy(v: &str, what: &str) -> Result<bool, String> {
        match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(format!("{what}: expected true or false, got '{v}'")),
        }
    }

    pub(super) struct Resolved {
        pub cfg: IrohConfig,
        pub key_file: Option<PathBuf>,
    }

    pub(super) fn resolve(
        flag_relays: &[String],
        flag_key: Option<PathBuf>,
    ) -> Result<Resolved, String> {
        let file = rustgit_host::keystore::config_dir().join("iroh.conf");
        let mut f = read_file(&file)?;
        // An environment variable replaces the file's value for that key.
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let mut over = |key: &str, var: &str, split: bool| {
            if let Some(v) = env(var) {
                let vals = if split {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    vec![v]
                };
                f.insert(key.to_string(), vals);
            }
        };
        over("relay", "RGIT_IROH_RELAY", true);
        over("public_relay", "RGIT_IROH_PUBLIC_RELAY", false);
        over("relay_ca", "RGIT_IROH_RELAY_CA", false);
        over("relay_token", "RGIT_IROH_RELAY_TOKEN", false);
        over("direct", "RGIT_IROH_DIRECT", false);
        over("bind", "RGIT_IROH_BIND", false);
        over("key_file", "RGIT_IROH_KEY_FILE", false);
        over("connect_timeout", "RGIT_IROH_CONNECT_TIMEOUT", false);
        if !flag_relays.is_empty() {
            f.insert("relay".into(), flag_relays.to_vec());
        }
        let one = |k: &str| f.get(k).and_then(|v| v.last()).cloned();

        let mut cfg = IrohConfig::default();
        let relays = f.get("relay").cloned().unwrap_or_default();
        let public = one("public_relay")
            .map(|v| truthy(&v, "public_relay"))
            .transpose()?
            .unwrap_or(false);
        cfg.relay =
            match (relays.is_empty(), public) {
                (false, true) => return Err(
                    "relay and public_relay are both set: choose your own relay or the public ones"
                        .into(),
                ),
                (false, false) => RelaySetting::Custom(relays),
                (true, true) => RelaySetting::Public,
                (true, false) => RelaySetting::None,
            };
        if let Some(p) = one("relay_ca") {
            cfg.relay_ca = load_ca_pem(Path::new(&p)).map_err(|e| format!("relay_ca: {e}"))?;
        }
        cfg.relay_token = one("relay_token").filter(|t| !t.is_empty());
        if let Some(v) = one("direct") {
            cfg.direct = truthy(&v, "direct")?;
        }
        if let Some(v) = one("bind") {
            cfg.bind_addr = Some(
                v.parse()
                    .map_err(|_| format!("bind: expected ip:port, got '{v}'"))?,
            );
        }
        cfg.connect_timeout = Some(Duration::from_secs(
            one("connect_timeout")
                .map(|v| {
                    v.parse::<u64>()
                        .map_err(|_| format!("connect_timeout: expected seconds, got '{v}'"))
                })
                .transpose()?
                .unwrap_or(20)
                .max(1),
        ));
        Ok(Resolved {
            cfg,
            key_file: flag_key.or_else(|| one("key_file").map(PathBuf::from)),
        })
    }

    // ---- client ------------------------------------------------------------------------------

    pub fn transport(url: &str) -> Result<Box<dyn Transport>, String> {
        let parsed: IrohUrl = url
            .parse()
            .map_err(|e: rustgit_transport_iroh::UrlError| e.to_string())?;
        let Resolved { mut cfg, key_file } = resolve(&[], None)?;
        if let Some(kf) = key_file {
            cfg.secret_key =
                Some(load_or_create_secret_key(&kf).map_err(|e| format!("iroh key file: {e}"))?);
        }
        let t = IrohTransport::new(parsed, cfg).map_err(|e| e.to_string())?;
        let handle = t.status_handle();
        handle.observe(|s| {
            // JSON mode: a progress event for the UI. Human mode: one line on stderr.
            if out::is_json() {
                let mut v = status_json(s);
                v["event"] = json!("connection");
                out::event(v);
            } else if let Some(path) = &s.selected {
                eprintln!("iroh: {} via {path}", s.state.name());
            } else if s.state == rustgit_transport_iroh::PathState::Unreachable {
                eprintln!("iroh: host unreachable");
            }
        });
        *CURRENT.lock().unwrap() = Some(handle);
        Ok(Box::new(t))
    }

    // ---- host --------------------------------------------------------------------------------

    pub fn extend_listener(
        tcp: Box<dyn rustgit_protocol::Listener>,
        args: &ServeArgs,
        hosted_dir: &Path,
        open_access: bool,
    ) -> Result<Box<dyn rustgit_protocol::Listener>, Error> {
        if !args.iroh {
            return Ok(tcp);
        }
        if open_access {
            return Err(Error::Other(
                "--iroh cannot be combined with --insecure-open-access: Iroh makes the host reachable \
                 from the internet, so devices must authenticate"
                    .into(),
            ));
        }
        let other = |e: String| Error::Other(e);
        let Resolved { mut cfg, key_file } =
            resolve(&args.iroh_relay, args.iroh_key_file.clone()).map_err(other)?;
        let key_file = key_file.unwrap_or_else(|| hosted_dir.join(".rgit/host/iroh.key"));
        cfg.secret_key = Some(
            load_or_create_secret_key(&key_file)
                .map_err(|e| Error::Other(format!("iroh key file {}: {e}", key_file.display())))?,
        );
        let has_relay = !matches!(cfg.relay, RelaySetting::None);
        let l = IrohListener::bind(&cfg, None).map_err(|e| Error::Other(e.to_string()))?;
        let online = if has_relay {
            let ok = l.wait_online(Duration::from_secs(15));
            if !ok {
                eprintln!(
                    "warning: could not reach the relay within 15 s; the host keeps trying, but until it \
                     connects, devices can only reach it through direct addresses"
                );
            }
            ok
        } else {
            eprintln!(
                "note: no relay configured (see docs/relay.md): the iroh:// URL carries direct addresses \
                 only, so it works on a LAN or with a forwarded port, not across NATs"
            );
            false
        };
        // Without a relay the URL is useless unless it carries addresses.
        let url = l.join_url(args.iroh_hint_addrs || !has_relay);
        println!("Iroh endpoint {}\nIroh URL {url}", l.endpoint_id());
        eprintln!("Hand this URL to devices with: rgit invite create --hint '{url}'");
        out::event(json!({
            "event": "listening_iroh",
            "url": url,
            "endpoint_id": l.endpoint_id(),
            "relay_online": online,
            "relay": match &cfg.relay {
                RelaySetting::None => json!(null),
                RelaySetting::Public => json!("public"),
                RelaySetting::Custom(v) => json!(v),
            },
        }));
        Ok(Box::new(FanInListener::new(vec![tcp, Box::new(l)])))
    }
}
