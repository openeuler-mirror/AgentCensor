use crate::{CompiledPolicy, CompiledRules, RuleValue};
use censorguard_common::abi::{Net6LpmKey, Net6PortLpmKey, NetLpmKey, NetPortLpmKey};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, ToSocketAddrs};

pub type DnsCache = BTreeMap<String, Vec<IpAddr>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsResolution {
    pub domain: String,
    pub addresses: Vec<IpAddr>,
    pub stale: bool,
    pub error: Option<String>,
}

impl CompiledPolicy {
    #[must_use]
    pub fn resolve_dns(&self, previous: &DnsCache) -> (Self, DnsCache, Vec<DnsResolution>) {
        let domains: BTreeSet<_> = self
            .baseline
            .iter()
            .chain(self.groups.values())
            .flat_map(|rules| rules.network_domains.iter())
            .map(|rule| rule.domain.clone())
            .collect();

        let mut cache = BTreeMap::new();
        let mut status = Vec::new();
        for domain in domains {
            match resolve_addresses(&domain) {
                Ok(addresses) if !addresses.is_empty() => {
                    cache.insert(domain.clone(), addresses.clone());
                    status.push(DnsResolution {
                        domain,
                        addresses,
                        stale: false,
                        error: None,
                    });
                }
                result => {
                    let addresses = previous.get(&domain).cloned().unwrap_or_default();
                    if !addresses.is_empty() {
                        cache.insert(domain.clone(), addresses.clone());
                    }
                    status.push(DnsResolution {
                        domain,
                        addresses,
                        stale: true,
                        error: result
                            .err()
                            .or(Some("resolver returned no IP addresses".into())),
                    });
                }
            }
        }

        let mut resolved = self.clone();
        if let Some(baseline) = resolved.baseline.as_mut() {
            apply_cache(baseline, &cache);
        }
        for rules in resolved.groups.values_mut() {
            apply_cache(rules, &cache);
        }
        resolved.rebuild_combinations();
        (resolved, cache, status)
    }
}

fn resolve_addresses(domain: &str) -> Result<Vec<IpAddr>, String> {
    let mut addresses: Vec<_> = (domain, 0)
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .map(|address| address.ip())
        .collect();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

fn apply_cache(rules: &mut CompiledRules, cache: &DnsCache) {
    rules.network = rules.network_static.clone();
    rules.network_ports = rules.network_ports_static.clone();
    rules.network6 = rules.network6_static.clone();
    rules.network6_ports = rules.network6_ports_static.clone();
    for rule in &rules.network_domains {
        let Some(addresses) = cache.get(&rule.domain) else {
            continue;
        };
        for address in addresses {
            match (address, rule.port) {
                (IpAddr::V4(address), Some(port)) => {
                    rules
                        .network_ports
                        .entry(NetPortLpmKey {
                            prefix_len: 48,
                            port,
                            addr: address.octets(),
                            pad: 0,
                        })
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action: rule.action as u8,
                            audit: rule.audit,
                            version: 0,
                        });
                }
                (IpAddr::V4(address), None) => {
                    rules
                        .network
                        .entry(NetLpmKey {
                            prefix_len: 32,
                            addr: u32::from_le_bytes(address.octets()),
                            port: 0,
                            pad: 0,
                        })
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action: rule.action as u8,
                            audit: rule.audit,
                            version: 0,
                        });
                }
                (IpAddr::V6(address), Some(port)) => {
                    rules
                        .network6_ports
                        .entry(Net6PortLpmKey {
                            prefix_len: 144,
                            port,
                            addr: address.octets(),
                            pad: 0,
                        })
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action: rule.action as u8,
                            audit: rule.audit,
                            version: 0,
                        });
                }
                (IpAddr::V6(address), None) => {
                    rules
                        .network6
                        .entry(Net6LpmKey {
                            prefix_len: 128,
                            addr: address.octets(),
                        })
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action: rule.action as u8,
                            audit: rule.audit,
                            version: 0,
                        });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_yaml;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn dns_refresh_preserves_static_rules_and_merges_conflicting_denies()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = compile_yaml(
            b"rules:\n  - net deny 127.0.0.1\n  - net deny 127.0.0.1:443\n  - net deny ::1\n  - net deny [::1]:443\n  - net allow+audit example.test\n  - net allow+audit example.test:443\n",
        )?;
        let mut rules = policy.baseline.clone().ok_or("missing baseline")?;
        let cache = DnsCache::from([(
            "example.test".into(),
            vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        )]);
        apply_cache(&mut rules, &cache);
        for values in [
            rules.network.values().collect::<Vec<_>>(),
            rules.network_ports.values().collect(),
            rules.network6.values().collect(),
            rules.network6_ports.values().collect(),
        ] {
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].action, 0);
            assert!(values[0].audit);
        }
        // Removing DNS addresses on a later refresh must still leave static denies.
        apply_cache(&mut rules, &DnsCache::new());
        assert_eq!(rules.network, rules.network_static);
        assert_eq!(rules.network_ports, rules.network_ports_static);
        assert_eq!(rules.network6, rules.network6_static);
        assert_eq!(rules.network6_ports, rules.network6_ports_static);
        let static_only = compile_yaml(b"rules:\n  - net deny 10.0.0.0/8\n")?;
        let (resolved, _, _) = static_only.resolve_dns(&DnsCache::new());
        assert_eq!(resolved.baseline, static_only.baseline);
        Ok(())
    }

    #[test]
    fn failed_resolution_keeps_previous_addresses() -> Result<(), crate::PolicyError> {
        let policy = compile_yaml(
            b"groups:\n  net:\n    rules:\n      - net deny definitely.invalid:443\n",
        )?;
        let mut previous = DnsCache::new();
        previous.insert(
            "definitely.invalid".into(),
            vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))],
        );
        let (resolved, cache, status) = policy.resolve_dns(&previous);
        assert_eq!(cache, previous);
        assert!(status[0].stale);
        assert_eq!(resolved.groups["net"].network_ports.len(), 1);
        Ok(())
    }

    #[test]
    fn stale_cache_preserves_ipv4_and_ipv6() -> Result<(), crate::PolicyError> {
        let policy = compile_yaml(
            b"groups:\n  net:\n    rules:\n      - net deny definitely.invalid:443\n",
        )?;
        let mut previous = DnsCache::new();
        previous.insert(
            "definitely.invalid".into(),
            vec![
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        );
        let (resolved, cache, status) = policy.resolve_dns(&previous);
        assert_eq!(cache, previous);
        assert!(status[0].stale);
        assert_eq!(resolved.groups["net"].network_ports.len(), 1);
        assert_eq!(resolved.groups["net"].network6_ports.len(), 1);
        Ok(())
    }
}
