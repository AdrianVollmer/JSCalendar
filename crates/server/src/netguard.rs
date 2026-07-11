//! Blocks outbound requests to private/internal network destinations.
//!
//! The ICS-subscription feature has the server fetch a URL any
//! authenticated user supplies (not just admins) — without this check,
//! that's a textbook SSRF primitive: point a subscription at
//! `http://169.254.169.254/...` (cloud instance metadata) or an internal
//! service's address and the server will request it on the attacker's
//! behalf, with both fetch errors and successfully-parsed content
//! reflected back to them.
//!
//! DNS resolution happens once here and the check applies to every
//! resolved address, but the actual HTTP client resolves the hostname
//! again independently when it connects — a DNS answer that changes
//! between these two lookups (a "rebinding" attack) could still slip a
//! private address past this check. That residual gap needs a custom
//! connector pinned to the addresses we already validated to close
//! completely; what's here still stops the untargeted, common case (a
//! user pointing a feed URL directly at a private/link-local literal or a
//! hostname that always resolves privately, e.g. `metadata.google.internal`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// True if `ip` is safe to let a user-supplied URL connect to. Deliberately
/// conservative: anything not obviously routable public internet is
/// rejected, including ranges `std` doesn't classify as "private" but that
/// still commonly point at internal infrastructure (carrier-grade NAT,
/// benchmarking).
pub fn is_public_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return false;
            }
            if v6.is_unique_local() || v6.is_unicast_link_local() {
                return false;
            }
            match v6.to_ipv4_mapped() {
                Some(v4) => is_public_v4(v4),
                None => true,
            }
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    if v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_documentation()
    {
        return false;
    }
    let [a, b, ..] = v4.octets();
    // 100.64.0.0/10 — carrier-grade NAT (RFC 6598), often used for
    // internal cloud networking; not covered by `is_private()`.
    if a == 100 && (64..128).contains(&b) {
        return false;
    }
    // 198.18.0.0/15 — benchmarking (RFC 2544).
    if a == 198 && (b == 18 || b == 19) {
        return false;
    }
    true
}

/// Resolves `host:port` and returns an error naming the offending address
/// if any resolved address isn't public. Rejecting the whole hostname when
/// *any* answer is private (rather than just filtering them out) means a
/// host that resolves to a mix of public and private addresses can't be
/// used to reach the private one by chance/retry.
pub async fn ensure_resolves_publicly(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("could not resolve host: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err("host did not resolve to any address".to_string());
    }
    for addr in &addrs {
        if !is_public_addr(addr.ip()) {
            return Err(format!(
                "refusing to connect to a private/internal address ({})",
                addr.ip()
            ));
        }
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_common_private_ranges() {
        let private = [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // carrier-grade NAT
            "198.18.0.1",      // benchmarking
            "0.0.0.0",
            "224.0.0.1", // multicast
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:169.254.169.254", // IPv4-mapped metadata address
        ];
        for ip in private {
            let addr: IpAddr = ip.parse().unwrap();
            assert!(!is_public_addr(addr), "{ip} should be rejected");
        }
    }

    #[test]
    fn accepts_public_addresses() {
        let public = [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ];
        for ip in public {
            let addr: IpAddr = ip.parse().unwrap();
            assert!(is_public_addr(addr), "{ip} should be accepted");
        }
    }
}
