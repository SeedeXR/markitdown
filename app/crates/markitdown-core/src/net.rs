//! Hardened HTTP for the `net` feature.
//!
//! Two concerns this centralizes for every outbound request:
//! * **Timeouts** — a global wall-clock cap so a slow/hanging server can never
//!   wedge a conversion thread indefinitely (slowloris).
//! * **SSRF guard** — when fetching an *untrusted* URL (a user/MCP-supplied
//!   document URL, or an attacker-controlled link inside a page such as a
//!   YouTube caption `baseUrl`), the resolver refuses to connect to
//!   private / loopback / link-local / cloud-metadata addresses. The guard runs
//!   inside the DNS resolver, so it is consulted again on **every redirect
//!   hop** — a benign URL cannot 302 its way to `169.254.169.254`.
//!
//! Endpoints the user *configures themselves* (a local LLM at
//! `http://localhost:11434`) use [`trusted_agent`], which keeps the timeout but
//! not the SSRF guard. Set `MARKITDOWN_ALLOW_LOCAL_URLS=1` to disable the guard
//! for the untrusted path too (e.g. converting genuinely-internal URLs).

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, Error};

/// Wall-clock cap for a whole request (connect + transfer).
const TIMEOUT_SECS: u64 = 30;
const ALLOW_LOCAL_ENV: &str = "MARKITDOWN_ALLOW_LOCAL_URLS";

fn allow_local() -> bool {
    std::env::var(ALLOW_LOCAL_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// True for addresses that could reach the host's own network or a cloud
/// metadata service — i.e. anything an SSRF would want.
fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254.0.0/16 (incl. cloud metadata)
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0 // 0.0.0.0/8
                // 100.64.0.0/10 carrier-grade NAT / shared address space
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            // IPv4-mapped (::ffff:a.b.c.d) — apply the v4 rules.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            (seg0 & 0xfe00) == 0xfc00      // fc00::/7  unique-local
                || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// A resolver that drops blocked addresses; if a host resolves only to blocked
/// addresses the request fails with `HostNotFound`.
#[derive(Debug)]
struct SafeResolver;

impl Resolver for SafeResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let host = uri.host().ok_or(Error::HostNotFound)?;
        let port = uri
            .port_u16()
            .or_else(|| match uri.scheme_str() {
                Some("https") => Some(443),
                Some("http") => Some(80),
                _ => None,
            })
            .ok_or(Error::HostNotFound)?;
        // `host()` keeps the brackets on an IPv6 literal; strip for resolution.
        let host = host.trim_start_matches('[').trim_end_matches(']');

        let allow = allow_local();
        let mut result = self.empty();
        // MAX_ADDRS is 16; cap pushes so the backing ArrayVec can't overflow.
        for addr in (host, port)
            .to_socket_addrs()
            .map_err(|_| Error::HostNotFound)?
            .take(16)
        {
            if !allow && is_blocked(addr.ip()) {
                continue;
            }
            result.push(addr);
        }
        if result.is_empty() {
            Err(Error::HostNotFound)
        } else {
            Ok(result)
        }
    }
}

/// Redirect hops allowed before a fetch is abandoned. Enough for the usual
/// http→https→www→canonical chain, low enough that a redirect loop fails fast
/// with a clear error instead of burning the whole request timeout.
const MAX_REDIRECTS: u32 = 10;

fn config() -> Config {
    Config::builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .max_redirects(MAX_REDIRECTS)
        // Surface "too many redirects" as an error rather than handing back
        // the last 3xx response as if it were the document.
        .max_redirects_will_error(true)
        // Never replay Authorization onto a host we were redirected to.
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        .build()
}

/// Agent for fetching **untrusted** URLs: timeout + SSRF guard on every hop.
pub fn agent() -> Agent {
    Agent::with_parts(config(), DefaultConnector::new(), SafeResolver)
}

/// Agent for **user-configured** endpoints (e.g. a local LLM): timeout only, no
/// SSRF guard, so `http://localhost:…` works.
pub fn trusted_agent() -> Agent {
    Agent::new_with_config(config())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn blocks_internal_targets() {
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)))); // metadata
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_blocked(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)))); // CGNAT
        assert!(is_blocked(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_blocked("fd00::1".parse().unwrap())); // unique-local
        assert!(is_blocked("fe80::1".parse().unwrap())); // link-local
        assert!(is_blocked("::ffff:127.0.0.1".parse().unwrap())); // v4-mapped
    }

    #[test]
    fn allows_public_targets() {
        assert!(!is_blocked(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_blocked(IpAddr::V4(Ipv4Addr::new(140, 82, 121, 3)))); // github
        assert!(!is_blocked("2606:4700:4700::1111".parse().unwrap())); // cloudflare v6
    }
}
