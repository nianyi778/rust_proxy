//! Enforce the destination policy at the connector's actual DNS resolution,
//! so validation and connection cannot resolve to different addresses.
use hyper::client::connect::dns::Name;
use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
};
use tower_service::Service;
use url::{Host, Url};

pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            // Global unicast only. Deny mapped IPv4, NAT64, Teredo, 6to4,
            // documentation and special-purpose space rather than tunnelling IPv4.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

pub fn validate_target(url: &Url) -> io::Result<()> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(denied());
    }
    match url.host() {
        Some(Host::Ipv4(ip)) if is_public(ip.into()) => Ok(()),
        Some(Host::Ipv6(ip)) if is_public(ip.into()) => Ok(()),
        Some(Host::Domain(host)) => validate_hostname(host),
        _ => Err(denied()),
    }
}

fn validate_hostname(host: &str) -> io::Result<()> {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if !h.contains('.')
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
    {
        Err(denied())
    } else {
        Ok(())
    }
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "destination is not permitted",
    )
}

fn public_addresses(
    addrs: impl Iterator<Item = SocketAddr>,
) -> io::Result<std::vec::IntoIter<SocketAddr>> {
    let addrs: Vec<_> = addrs.collect();
    if addrs.is_empty() || addrs.iter().any(|addr| !is_public(addr.ip())) {
        return Err(denied());
    }
    Ok(addrs.into_iter())
}

#[derive(Clone)]
pub struct PublicResolver;
impl Service<Name> for PublicResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, name: Name) -> Self::Future {
        Box::pin(async move {
            validate_hostname(name.as_str())?;
            // The connector uses these exact addresses; no second DNS lookup.
            public_addresses(tokio::net::lookup_host((name.as_str(), 0)).await?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ssrf_targets_and_alternative_ip_encodings() {
        for target in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
            "http://0/",
            "http://169.254.169.254/opc/v1/identity/",
            "http://10.0.0.64/",
            "http://100.64.0.1/",
            "http://192.168.1.1/",
            "http://172.17.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[2002:7f00:1::]/",
            "http://localhost/",
            "http://localhost./",
            "http://foo.localhost/",
            "http://host.internal/",
            "http://host.local/",
            "http://user:pass@example.com/",
            "file:///etc/passwd",
        ] {
            assert!(
                validate_target(&Url::parse(target).unwrap()).is_err(),
                "accepted {target}"
            );
        }
    }
    #[test]
    fn accepts_public_video_origins() {
        for target in [
            "https://cdn.example.com/video.m3u8",
            "https://1.1.1.1/video.ts",
            "https://[2606:4700:4700::1111]/video.mp4",
        ] {
            assert!(
                validate_target(&Url::parse(target).unwrap()).is_ok(),
                "rejected {target}"
            );
        }
    }
    #[test]
    fn refuses_mixed_public_private_dns_answers() {
        let public: SocketAddr = "1.1.1.1:0".parse().unwrap();
        let private: SocketAddr = "127.0.0.1:0".parse().unwrap();
        assert!(public_addresses(vec![public, private].into_iter()).is_err());
        assert!(public_addresses(vec![private, public].into_iter()).is_err());
        assert!(public_addresses(vec![public].into_iter()).is_ok());
        assert!(public_addresses(Vec::new().into_iter()).is_err());
    }
}
