use crate::PolicyError;
use censorguard_common::abi::{Net6LpmKey, Net6PortLpmKey, NetLpmKey, NetPortLpmKey};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum NetworkAction {
    Deny = 0,
    Allow = 1,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DomainNetworkRule {
    pub action: NetworkAction,
    pub audit: bool,
    pub domain: String,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParsedNetworkRule {
    Address4 {
        action: NetworkAction,
        audit: bool,
        key: NetLpmKey,
    },
    Address4Port {
        action: NetworkAction,
        audit: bool,
        key: NetPortLpmKey,
    },
    Address6 {
        action: NetworkAction,
        audit: bool,
        key: Net6LpmKey,
    },
    Address6Port {
        action: NetworkAction,
        audit: bool,
        key: Net6PortLpmKey,
    },
    Domain(DomainNetworkRule),
}

pub(crate) fn parse_network_rule(rule: &str) -> Result<ParsedNetworkRule, PolicyError> {
    let fields: Vec<_> = rule.split_whitespace().collect();
    let (action, audit, target) = match fields.as_slice() {
        [target] if !matches!(*target, "allow" | "deny" | "allow+audit" | "deny+audit") => {
            (NetworkAction::Deny, false, *target)
        }
        [action_token, target] => {
            let lowered = action_token.to_ascii_lowercase();
            let mut parts = lowered.split('+');
            let action = match parts.next().unwrap_or_default() {
                "allow" => NetworkAction::Allow,
                "deny" => NetworkAction::Deny,
                _ => return network_error(rule, format!("unknown action {action_token:?}")),
            };
            let suffix = parts.next();
            let audit = matches!(suffix, Some("audit")) && parts.next().is_none();
            if lowered.contains('+') && !audit {
                return network_error(rule, "action suffix only supports +audit");
            }
            (action, audit, *target)
        }
        _ => return network_error(rule, "expected: [allow|deny] TARGET"),
    };

    let (host, port) = split_port(rule, target)?;
    let parsed = parse_address(rule, host)?;
    if target.starts_with('[') && !matches!(parsed, Some((IpAddr::V6(_), _))) {
        return network_error(rule, "bracketed host must be an IPv6 address or CIDR");
    }
    match parsed {
        Some((IpAddr::V4(address), prefix_len)) => {
            if let Some(port) = port {
                Ok(ParsedNetworkRule::Address4Port {
                    action,
                    audit,
                    key: NetPortLpmKey {
                        prefix_len: u32::from(prefix_len) + 16,
                        port,
                        addr: address.octets(),
                        pad: 0,
                    },
                })
            } else {
                Ok(ParsedNetworkRule::Address4 {
                    action,
                    audit,
                    key: NetLpmKey {
                        prefix_len: u32::from(prefix_len),
                        addr: u32::from_le_bytes(address.octets()),
                        port: 0,
                        pad: 0,
                    },
                })
            }
        }
        Some((IpAddr::V6(address), prefix_len)) => {
            if let Some(port) = port {
                Ok(ParsedNetworkRule::Address6Port {
                    action,
                    audit,
                    key: Net6PortLpmKey {
                        prefix_len: u32::from(prefix_len) + 16,
                        port,
                        addr: address.octets(),
                        pad: 0,
                    },
                })
            } else {
                Ok(ParsedNetworkRule::Address6 {
                    action,
                    audit,
                    key: Net6LpmKey {
                        prefix_len: u32::from(prefix_len),
                        addr: address.octets(),
                    },
                })
            }
        }
        None => {
            if !valid_domain(host) {
                return network_error(
                    rule,
                    "target is not an IPv4/IPv6 address, CIDR, or valid domain",
                );
            }
            Ok(ParsedNetworkRule::Domain(DomainNetworkRule {
                action,
                audit,
                domain: host.to_ascii_lowercase(),
                port,
            }))
        }
    }
}

fn split_port<'a>(rule: &str, target: &'a str) -> Result<(&'a str, Option<u16>), PolicyError> {
    if let Some(bracketed) = target.strip_prefix('[') {
        let Some(close) = bracketed.find(']') else {
            return network_error(rule, "missing closing bracket in IPv6 target");
        };
        let host = &bracketed[..close];
        let suffix = &bracketed[close + 1..];
        let Some(port) = suffix.strip_prefix(':') else {
            return network_error(rule, "bracketed IPv6 target requires :PORT");
        };
        if host.is_empty() || port.is_empty() {
            return network_error(rule, "IPv6 host and port must be non-empty");
        }
        return Ok((host, Some(parse_port(rule, port)?)));
    }

    if target.bytes().filter(|byte| *byte == b':').count() != 1 {
        return Ok((target, None));
    }
    let (host, suffix) = target
        .rsplit_once(':')
        .ok_or_else(|| network_reason(rule, "invalid host:port target"))?;
    if host.is_empty() || suffix.is_empty() {
        return network_error(rule, "host and port must be non-empty");
    }
    Ok((host, Some(parse_port(rule, suffix)?)))
}

fn parse_port(rule: &str, value: &str) -> Result<u16, PolicyError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| network_reason(rule, "port must be between 1 and 65535"))?;
    if port == 0 {
        return network_error(rule, "port must be between 1 and 65535");
    }
    Ok(port)
}

fn parse_address(rule: &str, host: &str) -> Result<Option<(IpAddr, u8)>, PolicyError> {
    if let Some((address, prefix)) = host.split_once('/') {
        let address = address
            .parse::<IpAddr>()
            .map_err(|_| network_reason(rule, "malformed IP address in CIDR"))?;
        let prefix_len = prefix
            .parse::<u8>()
            .map_err(|_| network_reason(rule, "CIDR prefix is not an integer"))?;
        return match address {
            IpAddr::V4(address) if prefix_len <= 32 => Ok(Some((
                IpAddr::V4(canonicalize4(address, prefix_len)),
                prefix_len,
            ))),
            IpAddr::V4(_) => network_error(rule, "IPv4 CIDR prefix must be between 0 and 32"),
            IpAddr::V6(address) if prefix_len <= 128 => Ok(Some((
                IpAddr::V6(canonicalize6(address, prefix_len)),
                prefix_len,
            ))),
            IpAddr::V6(_) => network_error(rule, "IPv6 CIDR prefix must be between 0 and 128"),
        };
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => Ok(Some((IpAddr::V4(address), 32))),
        Ok(IpAddr::V6(address)) => Ok(Some((IpAddr::V6(address), 128))),
        Err(_) if host.contains(':') || host.chars().all(|c| c.is_ascii_digit() || c == '.') => {
            network_error(rule, "malformed IP address")
        }
        Err(_) => Ok(None),
    }
}

fn canonicalize4(address: Ipv4Addr, prefix_len: u8) -> Ipv4Addr {
    let raw = u32::from_be_bytes(address.octets());
    let mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    Ipv4Addr::from((raw & mask).to_be_bytes())
}

fn canonicalize6(address: Ipv6Addr, prefix_len: u8) -> Ipv6Addr {
    let raw = u128::from_be_bytes(address.octets());
    let mask = if prefix_len == 0 {
        0
    } else {
        u128::MAX << (128 - prefix_len)
    };
    Ipv6Addr::from((raw & mask).to_be_bytes())
}

fn valid_domain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

fn network_reason(rule: &str, reason: impl Into<String>) -> PolicyError {
    PolicyError::Network {
        rule: rule.to_owned(),
        reason: reason.into(),
    }
}

fn network_error<T>(rule: &str, reason: impl Into<String>) -> Result<T, PolicyError> {
    Err(network_reason(rule, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_port_uses_port_first_prefix() -> Result<(), PolicyError> {
        let parsed = parse_network_rule("deny 10.7.9.1/8:22")?;
        assert_eq!(
            parsed,
            ParsedNetworkRule::Address4Port {
                action: NetworkAction::Deny,
                audit: false,
                key: NetPortLpmKey {
                    prefix_len: 24,
                    port: 22,
                    addr: [10, 0, 0, 0],
                    pad: 0,
                },
            }
        );
        Ok(())
    }

    #[test]
    fn ipv6_cidr_port_is_canonical_and_port_first() -> Result<(), PolicyError> {
        let parsed = parse_network_rule("deny [2001:db8:1234::1/32]:443")?;
        assert_eq!(
            parsed,
            ParsedNetworkRule::Address6Port {
                action: NetworkAction::Deny,
                audit: false,
                key: Net6PortLpmKey {
                    prefix_len: 48,
                    port: 443,
                    addr: Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0).octets(),
                    pad: 0,
                },
            }
        );
        Ok(())
    }

    #[test]
    fn bare_ipv6_has_no_port() -> Result<(), PolicyError> {
        assert_eq!(
            parse_network_rule("allow ::1")?,
            ParsedNetworkRule::Address6 {
                action: NetworkAction::Allow,
                audit: false,
                key: Net6LpmKey {
                    prefix_len: 128,
                    addr: Ipv6Addr::LOCALHOST.octets(),
                },
            }
        );
        Ok(())
    }

    #[test]
    fn domain_is_normalized() -> Result<(), PolicyError> {
        let parsed = parse_network_rule("allow API.Example.COM:443")?;
        assert_eq!(
            parsed,
            ParsedNetworkRule::Domain(DomainNetworkRule {
                action: NetworkAction::Allow,
                audit: false,
                domain: "api.example.com".into(),
                port: Some(443),
            })
        );
        Ok(())
    }
}
