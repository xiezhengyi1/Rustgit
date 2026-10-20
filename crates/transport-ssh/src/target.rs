//! `ssh://[user@]host[:port]/path[?jump=h1,h2]` parsed into validated, structured parts.
//!
//! Everything in an SSH destination can come from an invitation or a shared remote URL, so
//! nothing here is ever interpolated into a shell string on this machine. The values end up in
//! three places only: as separate `argv` entries of the local `ssh` process (after `--` or as
//! the value of an option), and, for the path, inside the one command string that the *remote*
//! shell must parse. The strict allow-lists below are what make that last step safe: they
//! reject quotes, `$`, backticks, `;`, `|`, `&`, newlines and everything else with meaning to a
//! shell, and a leading `-` is rejected everywhere so no value can be mistaken for an option.

use std::fmt;
use std::net::Ipv6Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetError(pub String);

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TargetError {}

fn bad<T>(m: impl Into<String>) -> Result<T, TargetError> {
    Err(TargetError(m.into()))
}

/// A host an SSH connection is made to or through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub user: Option<String>,
    /// DNS name, IPv4 literal, ssh_config alias, or an IPv6 literal *without* brackets.
    pub host: String,
    pub port: Option<u16>,
}

impl Hop {
    /// `[user@]host[:port]` as `ssh -J` expects it (IPv6 in brackets).
    pub fn jump_spec(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        if self.host.contains(':') {
            s.push('[');
            s.push_str(&self.host);
            s.push(']');
        } else {
            s.push_str(&self.host);
        }
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        s
    }

    fn display_authority(&self) -> String {
        self.jump_spec()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub hop: Hop,
    /// Directory of the hosted repository on the remote machine. Absolute, or relative to the
    /// remote user's home directory (from `ssh://host/~/rel`). Never starts with `-`.
    pub path: String,
    /// Jump hosts, in connection order (`ssh -J a,b`).
    pub jumps: Vec<Hop>,
}

const MAX_PATH: usize = 1024;

pub fn validate_user(u: &str) -> Result<(), TargetError> {
    let ok = !u.is_empty()
        && u.len() <= 64
        && !u.starts_with(['-', '.'])
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        bad(format!(
            "invalid SSH user name {u:?} (letters, digits, '.', '_' and '-' only; must not start with '-' or '.')"
        ))
    }
}

pub fn validate_host(h: &str) -> Result<(), TargetError> {
    if h.contains(':') {
        return match h.parse::<Ipv6Addr>() {
            Ok(_) => Ok(()),
            Err(_) => bad(format!("invalid IPv6 address {h:?}")),
        };
    }
    let ok = !h.is_empty()
        && h.len() <= 253
        && !h.starts_with(['-', '.'])
        && !h.ends_with('.')
        && !h.contains("..")
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        bad(format!(
            "invalid SSH host name {h:?} (letters, digits, '.', '_' and '-' only; must not start with '-' or '.')"
        ))
    }
}

fn parse_port(p: &str) -> Result<u16, TargetError> {
    match p.parse::<u16>() {
        Ok(n) if n != 0 && p.chars().all(|c| c.is_ascii_digit()) => Ok(n),
        _ => bad(format!("invalid port {p:?}")),
    }
}

/// `[user@]host[:port]`, host possibly `[v6]`.
pub fn parse_hop(s: &str) -> Result<Hop, TargetError> {
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => {
            validate_user(u)?;
            (Some(u.to_string()), r)
        }
        None => (None, s),
    };
    let (host, port) = if let Some(r) = rest.strip_prefix('[') {
        let Some((h, after)) = r.split_once(']') else {
            return bad(format!("unterminated '[' in {s:?}"));
        };
        if !h.contains(':') {
            return bad(format!("brackets are for IPv6 literals only: {s:?}"));
        }
        let port = match after {
            "" => None,
            a => match a.strip_prefix(':') {
                Some(p) => Some(parse_port(p)?),
                None => return bad(format!("unexpected text after ']' in {s:?}")),
            },
        };
        (h.to_string(), port)
    } else {
        match rest.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(parse_port(p)?)),
            None => (rest.to_string(), None),
        }
    };
    validate_host(&host)?;
    Ok(Hop { user, host, port })
}

/// Remote directory: conservative allow-list (Unicode letters/digits and ` ._-/+@:,=~`).
pub fn validate_path(p: &str) -> Result<(), TargetError> {
    if p.is_empty() || p.len() > MAX_PATH {
        return bad("the remote path is empty or too long");
    }
    if p.starts_with('-') {
        return bad("the remote path must not start with '-'");
    }
    if p.split('/').any(|seg| seg == "..") {
        return bad("the remote path must not contain '..'");
    }
    if let Some(c) = p
        .chars()
        .find(|c| !(c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '/' | '+' | '@' | ':' | ',' | '=' | '~')))
    {
        return bad(format!(
            "the remote path contains {c:?}, which is not allowed (letters, digits, space and ._-/+@:,=~ only)"
        ));
    }
    Ok(())
}

fn percent_decode(s: &str) -> Result<String, TargetError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            match hex {
                Some(v) => {
                    out.push(v);
                    i += 3;
                }
                None => return bad("malformed percent-escape in the SSH URL"),
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).or_else(|_| bad("the SSH URL path is not valid UTF-8"))
}

impl SshTarget {
    pub fn parse(url: &str) -> Result<SshTarget, TargetError> {
        let Some(rest) = url.strip_prefix("ssh://") else {
            return bad(format!("not an ssh:// URL: {url:?}"));
        };
        if rest.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return bad("an SSH URL must not contain whitespace or control characters (encode spaces in the path as %20)");
        }
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        if rest.contains('#') {
            return bad("SSH URLs cannot have a fragment");
        }
        let Some((authority, raw_path)) = rest.split_once('/') else {
            return bad("an SSH URL needs a path: ssh://[user@]host[:port]/path/to/project.rgit");
        };
        if authority.contains('%') {
            return bad("the host part of an SSH URL cannot be percent-encoded");
        }
        let hop = parse_hop(authority)?;
        let decoded = percent_decode(raw_path)?;
        let path = if let Some(home_rel) = decoded.strip_prefix("~/") {
            home_rel.to_string()
        } else {
            format!("/{decoded}")
        };
        if path == "/" || path.is_empty() {
            return bad("an SSH URL needs a project path after the host");
        }
        validate_path(&path)?;
        let mut jumps = Vec::new();
        if let Some(q) = query {
            for pair in q.split('&').filter(|p| !p.is_empty()) {
                match pair.split_once('=') {
                    Some(("jump", v)) if !v.is_empty() => {
                        for h in v.split(',') {
                            jumps.push(parse_hop(h)?);
                        }
                    }
                    _ => {
                        return bad(format!(
                            "unsupported SSH URL option {pair:?} (only ?jump=host1,host2 exists)"
                        ))
                    }
                }
            }
        }
        if jumps.len() > 8 {
            return bad("too many jump hosts (at most 8)");
        }
        Ok(SshTarget { hop, path, jumps })
    }

    /// Human-readable destination (no secrets exist in it); used by `Transport::describe`.
    pub fn describe(&self) -> String {
        let mut s = format!("ssh://{}", self.hop.display_authority());
        if self.path.starts_with('/') {
            s.push_str(&self.path);
        } else {
            s.push_str("/~/");
            s.push_str(&self.path);
        }
        if !self.jumps.is_empty() {
            s.push_str("?jump=");
            s.push_str(
                &self
                    .jumps
                    .iter()
                    .map(Hop::jump_spec)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        s
    }
}

/// Quotes `s` for a POSIX shell only if it contains anything but characters that are safe in
/// every shell (including Windows `cmd.exe`, which does not understand single quotes).
/// `s` has already passed [`validate_path`] / [`validate_program`], so the escape of `'` is
/// belt and braces.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | ':' | ',' | '=' | '~'))
    {
        return s.to_string();
    }
    let mut q = String::with_capacity(s.len() + 2);
    q.push('\'');
    for c in s.chars() {
        if c == '\'' {
            q.push_str("'\\''");
        } else {
            q.push(c);
        }
    }
    q.push('\'');
    q
}

/// The name or path of the `rgit` executable on the remote machine.
pub fn validate_program(p: &str) -> Result<(), TargetError> {
    let ok = !p.is_empty()
        && p.len() <= 256
        && !p.starts_with('-')
        && p
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | ':' | ',' | '=' | '~'));
    if ok {
        Ok(())
    } else {
        bad(format!(
            "invalid remote program {p:?} (letters, digits and ._-/+@:,=~ only; must not start with '-')"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_forms() {
        let t = SshTarget::parse("ssh://ann@host.example:2222/srv/proj.rgit").unwrap();
        assert_eq!(t.hop.user.as_deref(), Some("ann"));
        assert_eq!(t.hop.host, "host.example");
        assert_eq!(t.hop.port, Some(2222));
        assert_eq!(t.path, "/srv/proj.rgit");
        assert!(t.jumps.is_empty());

        let t = SshTarget::parse("ssh://host/~/projects/a%20b?jump=bob@bastion:2200,[::1]:22").unwrap();
        assert_eq!(t.path, "projects/a b");
        assert_eq!(t.jumps.len(), 2);
        assert_eq!(t.jumps[0].jump_spec(), "bob@bastion:2200");
        assert_eq!(t.jumps[1].jump_spec(), "[::1]:22");

        let t = SshTarget::parse("ssh://[fe80::1]:22/x").unwrap();
        assert_eq!(t.hop.host, "fe80::1");
        assert_eq!(
            t.describe(),
            "ssh://[fe80::1]:22/x",
        );
    }

    #[test]
    fn rejects_option_like_and_shell_like_hosts() {
        for url in [
            "ssh://-oProxyCommand=touch%20pwned/x",
            "ssh://-oProxyCommand=touch/x",
            "ssh://-F/x",
            "ssh://user@-oProxyCommand=id/x",
            "ssh://-user@host/x",
            "ssh://host name/x",
            "ssh://host%20name/x",
            "ssh://host\nname/x",
            "ssh://host;touch pwned/x",
            "ssh://host;id/x",
            "ssh://`id`/x",
            "ssh://$(id)/x",
            "ssh://host|nc/x",
            "ssh://a..b/x",
            "ssh://.hidden/x",
            "ssh://host./x",
            "ssh://u ser@host/x",
            "ssh://us\ner@host/x",
            "ssh://host:0/x",
            "ssh://host:99999/x",
            "ssh://host:22abc/x",
            "ssh://[host]/x",
            "ssh://[::1/x",
            "ssh://*/x",
            "ssh:///x",
            "ssh://host",
            "ssh://host/",
        ] {
            assert!(SshTarget::parse(url).is_err(), "should reject {url:?}");
        }
    }

    #[test]
    fn rejects_hostile_paths() {
        for url in [
            "ssh://host/x;touch%20pwned",
            "ssh://host/x$(id)",
            "ssh://host/x%60id%60",
            "ssh://host/x%27%3Bid%3B%27",
            "ssh://host/x%22",
            "ssh://host/x%0Aid",
            "ssh://host/x%00",
            "ssh://host/x%7Cid",
            "ssh://host/x%26id",
            "ssh://host/x%5Cy",
            "ssh://host/x%3Ey",
            "ssh://host/x%2A",
            "ssh://host/x%25",
            "ssh://host/../etc",
            "ssh://host/a/../b",
            "ssh://host/~/-rf",
            "ssh://host/~/..",
            "ssh://host/x%zz",
            "ssh://host/x#frag",
            "ssh://host/x y",
            "ssh://host/x\ty",
        ] {
            assert!(SshTarget::parse(url).is_err(), "should reject {url:?}");
        }
    }

    #[test]
    fn rejects_unknown_query_options_and_hostile_jumps() {
        for url in [
            "ssh://host/x?proxycommand=id",
            "ssh://host/x?jump=",
            "ssh://host/x?jump=-oProxyCommand=id",
            "ssh://host/x?jump=a b",
            "ssh://host/x?jump=a,,b",
            "ssh://host/x?jump=a;b",
            "ssh://host/x?jump=ok&o=-F",
            "ssh://host/x?jump=a,b,c,d,e,f,g,h,i",
        ] {
            assert!(SshTarget::parse(url).is_err(), "should reject {url:?}");
        }
    }

    #[test]
    fn quoting_is_minimal_and_safe() {
        assert_eq!(shell_quote("/srv/proj.rgit"), "/srv/proj.rgit");
        assert_eq!(shell_quote("/srv/my proj"), "'/srv/my proj'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn remote_program_is_validated() {
        assert!(validate_program("rgit").is_ok());
        assert!(validate_program("/usr/local/bin/rgit").is_ok());
        for p in ["", "-x", "rgit;id", "rgit id", "$(id)", "r\ngit"] {
            assert!(validate_program(p).is_err(), "{p:?}");
        }
    }

    #[test]
    fn unicode_paths_are_allowed() {
        let t = SshTarget::parse("ssh://host/%E9%A1%B9%E7%9B%AE/a").unwrap();
        assert_eq!(t.path, "/项目/a");
    }
}
