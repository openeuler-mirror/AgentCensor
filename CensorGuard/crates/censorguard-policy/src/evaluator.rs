use crate::{CompiledPolicy, CompiledRules, GroupPolicy, NetworkAction};
use censorguard_common::protocol::{FileIntentOperation, IntentDecision, SecurityIntent};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn evaluate_intents(
    policy: &CompiledPolicy,
    group: &str,
    enables: [bool; 3],
    intents: &[SecurityIntent],
) -> Vec<IntentDecision> {
    intents
        .iter()
        .map(|intent| evaluate_intent(policy, group, enables, intent))
        .collect()
}

fn evaluate_intent(
    policy: &CompiledPolicy,
    group: &str,
    enables: [bool; 3],
    intent: &SecurityIntent,
) -> IntentDecision {
    match intent {
        SecurityIntent::File {
            operation,
            path,
            destination,
        } => evaluate_file(
            policy,
            group,
            enables,
            *operation,
            path,
            destination.as_deref(),
        ),
        SecurityIntent::Exec { executable, argv } => {
            evaluate_exec(policy, group, enables, executable, argv)
        }
        SecurityIntent::Network { host, port, .. } => {
            evaluate_network(policy, group, enables, host, *port)
        }
        SecurityIntent::UnknownTool { tool_name } => deny(
            Some(format!("unknown-tool:{tool_name}")),
            "unknown in-process tool requires an explicit profile decision",
        ),
    }
}

fn evaluate_file(
    policy: &CompiledPolicy,
    group: &str,
    enables: [bool; 3],
    operation: FileIntentOperation,
    path: &str,
    destination: Option<&str>,
) -> IntentDecision {
    if !enables[0] {
        return allow();
    }
    let operation_name = match operation {
        FileIntentOperation::Read => "read",
        FileIntentOperation::Write => "write",
        FileIntentOperation::Delete => "delete",
        FileIntentOperation::Rename => "rename",
        FileIntentOperation::Attr => "attr",
    };
    for (name, definition) in policy_definitions(policy, group) {
        if let Some(rule) = matching_file_rule(definition, path, operation_name).or_else(|| {
            destination.and_then(|target| matching_file_rule(definition, target, operation_name))
        }) {
            return deny(
                Some(format!("{name}:{rule}")),
                "file intent matched deny rule",
            );
        }
    }
    allow()
}

fn matching_file_rule<'a>(
    definition: &'a GroupPolicy,
    path: &str,
    operation: &str,
) -> Option<&'a str> {
    definition
        .rules
        .iter()
        .find(|line| {
            let f: Vec<_> = line.split_whitespace().collect();
            f.len() >= 3
                && f[0] == "file"
                && f[1].starts_with("deny")
                && path_matches(f[2], path)
                && (f.get(3).is_none() || f[3] == "none" || f[3].contains(operation))
        })
        .map(String::as_str)
}

fn path_matches(rule: &str, target: &str) -> bool {
    target == rule
        || target
            .strip_prefix(rule)
            .is_some_and(|suffix| rule.ends_with('/') || suffix.starts_with('/'))
}

fn evaluate_exec(
    policy: &CompiledPolicy,
    group: &str,
    enables: [bool; 3],
    executable: &str,
    argv: &[String],
) -> IntentDecision {
    if !enables[1] {
        return allow();
    }
    let tokens = normalized_exec_tokens(executable, argv);
    for (name, definition) in policy_definitions(policy, group) {
        for rule in &definition.rules {
            let fields: Vec<_> = rule.split_whitespace().collect();
            if fields.len() == 3
                && fields[0] == "exec"
                && fields[1].starts_with("deny")
                && fields[2] == executable
            {
                return deny(Some(format!("{name}:{rule}")), "unified exec rule matched");
            }
            if fields.len() >= 3
                && fields[0] == "exec"
                && fields[1].starts_with("deny")
                && exact_argument_key(&fields[2..], &tokens)
            {
                return deny(
                    Some(format!("{name}:{rule}")),
                    "unified exec argv rule matched",
                );
            }
        }
    }
    allow()
}

fn normalized_exec_tokens<'a>(executable: &'a str, argv: &'a [String]) -> Vec<&'a str> {
    let mut tokens = Vec::with_capacity(4);
    if argv.first().is_none_or(|first| first != executable) {
        tokens.push(executable);
    }
    tokens.extend(argv.iter().map(String::as_str));
    tokens.truncate(4);
    tokens
}

fn exact_argument_key(rule: &[&str], target: &[&str]) -> bool {
    rule.len() <= target.len() && rule.iter().zip(target).all(|(left, right)| left == right)
}

fn evaluate_network(
    policy: &CompiledPolicy,
    group: &str,
    enables: [bool; 3],
    host: &str,
    port: Option<u16>,
) -> IntentDecision {
    if !enables[2] {
        return allow();
    }
    let baseline = policy.baseline.as_ref();
    let group_rules = group.split('+').filter_map(|name| policy.groups.get(name));
    let mut allowed = None;
    for rules in baseline.into_iter().chain(group_rules) {
        let decision = if let Ok(address) = host.parse::<IpAddr>() {
            address_decision(Some(rules), None, address, port)
        } else {
            let normalized = host.trim_end_matches('.').to_ascii_lowercase();
            rules
                .network_domains
                .iter()
                .filter(|rule| {
                    rule.domain == normalized && (rule.port.is_none() || rule.port == port)
                })
                .max_by_key(|rule| (rule.port.is_some(), rule.action == NetworkAction::Deny))
                .map(|rule| {
                    (
                        rule.action,
                        format!(
                            "{}{}",
                            rule.domain,
                            rule.port
                                .map_or_else(String::new, |port| format!(":{port}"))
                        ),
                    )
                })
        };
        if let Some((action, rule)) = decision {
            if action == NetworkAction::Deny {
                return network_decision(action, rule);
            }
            allowed = Some(network_decision(action, rule));
        }
    }
    allowed.unwrap_or_else(allow)
}

fn address_decision(
    group: Option<&CompiledRules>,
    baseline: Option<&CompiledRules>,
    address: IpAddr,
    port: Option<u16>,
) -> Option<(NetworkAction, String)> {
    for rules in [group, baseline].into_iter().flatten() {
        if let Some(port) = port {
            let matched = match address {
                IpAddr::V4(address) => rules
                    .network_ports
                    .iter()
                    .filter(|(key, _)| {
                        key.port == port && matches_v4(address, key.addr, key.prefix_len - 16)
                    })
                    .max_by_key(|(key, _)| key.prefix_len)
                    .map(|(key, value)| {
                        (
                            if value.action == 0 {
                                NetworkAction::Deny
                            } else {
                                NetworkAction::Allow
                            },
                            format!("ipv4/{}/{port}", key.prefix_len - 16),
                        )
                    }),
                IpAddr::V6(address) => rules
                    .network6_ports
                    .iter()
                    .filter(|(key, _)| {
                        key.port == port && matches_v6(address, key.addr, key.prefix_len - 16)
                    })
                    .max_by_key(|(key, _)| key.prefix_len)
                    .map(|(key, value)| {
                        (
                            if value.action == 0 {
                                NetworkAction::Deny
                            } else {
                                NetworkAction::Allow
                            },
                            format!("ipv6/{}/{port}", key.prefix_len - 16),
                        )
                    }),
            };
            if matched.is_some() {
                return matched;
            }
        }
    }
    for rules in [group, baseline].into_iter().flatten() {
        let matched = match address {
            IpAddr::V4(address) => rules
                .network
                .iter()
                .filter(|(key, _)| matches_v4(address, key.addr.to_le_bytes(), key.prefix_len))
                .max_by_key(|(key, _)| key.prefix_len)
                .map(|(key, value)| {
                    (
                        if value.action == 0 {
                            NetworkAction::Deny
                        } else {
                            NetworkAction::Allow
                        },
                        format!("ipv4/{}", key.prefix_len),
                    )
                }),
            IpAddr::V6(address) => rules
                .network6
                .iter()
                .filter(|(key, _)| matches_v6(address, key.addr, key.prefix_len))
                .max_by_key(|(key, _)| key.prefix_len)
                .map(|(key, value)| {
                    (
                        if value.action == 0 {
                            NetworkAction::Deny
                        } else {
                            NetworkAction::Allow
                        },
                        format!("ipv6/{}", key.prefix_len),
                    )
                }),
        };
        if matched.is_some() {
            return matched;
        }
    }
    None
}

fn matches_v4(address: Ipv4Addr, network: [u8; 4], prefix: u32) -> bool {
    let address = u32::from_be_bytes(address.octets());
    let network = u32::from_be_bytes(network);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    address & mask == network & mask
}

fn matches_v6(address: Ipv6Addr, network: [u8; 16], prefix: u32) -> bool {
    let address = u128::from_be_bytes(address.octets());
    let network = u128::from_be_bytes(network);
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    address & mask == network & mask
}

fn policy_definitions<'a>(
    policy: &'a CompiledPolicy,
    group: &'a str,
) -> impl Iterator<Item = (&'a str, &'a GroupPolicy)> {
    group
        .split('+')
        .filter_map(|name| policy.group_definitions.get(name).map(|item| (name, item)))
        .chain(
            policy
                .baseline_definition
                .as_ref()
                .map(|item| ("__base__", item)),
        )
}

fn network_decision(action: NetworkAction, rule: String) -> IntentDecision {
    if action == NetworkAction::Deny {
        deny(Some(rule), "network intent matched deny rule")
    } else {
        IntentDecision {
            allowed: true,
            rule: Some(rule),
            reason: Some("network intent matched allow rule".into()),
        }
    }
}

fn allow() -> IntentDecision {
    IntentDecision {
        allowed: true,
        rule: None,
        reason: None,
    }
}

fn deny(rule: Option<String>, reason: impl Into<String>) -> IntentDecision {
    IntentDecision {
        allowed: false,
        rule,
        reason: Some(reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyError, compile_yaml};

    const ENABLES: [bool; 3] = [true, true, true];

    #[test]
    fn composite_network_maps_preserve_independent_layer_decisions()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = compile_yaml(b"rules: []")?
            .replace_group_yaml(
                "a",
                br#"rules:
  - net deny 10.0.0.0/8
  - net allow 10.1.0.0/16:443
  - net deny 2001:db8::/32
  - net allow [2001:db8:1::/48]:443
  - net deny 192.0.2.1/32
  - net deny 2001:db9::1/128
"#,
            )?
            .replace_group_yaml(
                "b",
                br#"rules:
  - net allow 10.0.0.0/8:80
  - net deny 10.1.2.0/24
  - net allow [2001:db8::/32]:80
  - net deny 2001:db8:1:2::/64
  - net deny 198.51.100.0/24:8443
  - net allow 198.51.100.42/32:8443
"#,
            )?
            .bind_domain("agent", "a", &[])?
            .bind_domain("agent", "b", &[])?;
        let composite = &policy.groups["a+b"];
        for host in [
            "9.255.255.255",
            "10.0.0.0",
            "10.1.0.0",
            "10.1.1.255",
            "10.1.2.0",
            "10.1.2.255",
            "10.1.3.0",
            "10.255.255.255",
            "11.0.0.0",
            "192.0.2.0",
            "192.0.2.1",
            "192.0.2.2",
            "198.51.100.41",
            "198.51.100.42",
            "198.51.100.43",
            "::",
            "2001:db8::",
            "2001:db8:1:1::1",
            "2001:db8:1:2::",
            "2001:db8:1:2:ffff:ffff:ffff:ffff",
            "2001:db8:1:3::",
            "2001:db9::",
            "2001:db9::1",
            "2001:db9::2",
            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        ] {
            let address = host.parse()?;
            for port in [
                None,
                Some(0),
                Some(22),
                Some(80),
                Some(443),
                Some(8443),
                Some(65535),
            ] {
                let expected_deny = ["a", "b"].iter().any(|group| {
                    address_decision(Some(&policy.groups[*group]), None, address, port)
                        .is_some_and(|(action, _)| action == NetworkAction::Deny)
                });
                let actual_deny = address_decision(Some(composite), None, address, port)
                    .is_some_and(|(action, _)| action == NetworkAction::Deny);
                assert_eq!(actual_deny, expected_deny, "{host}:{port:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn baseline_and_group_are_independent_layers() -> Result<(), PolicyError> {
        let initial = compile_yaml(
            br#"
rules:
  - file deny /tmp/base-secret
  - exec deny /usr/bin/id
  - net deny 10.0.0.0/8
  - net deny 2001:db8::/32
  - net deny example.com
"#,
        )?;
        let grouped = initial.replace_group_yaml(
            "worker",
            br#"
rules:
  - file allow /tmp/base-secret
  - file deny /tmp/group-secret
  - exec allow /usr/bin/id
  - exec deny /usr/bin/true
  - net allow 10.1.0.0/16:443
  - net allow [2001:db8::/64]:443
  - net allow example.com:443
  - net deny 192.0.2.0/24
"#,
        )?;
        let bound = grouped.bind_domain("agent", "worker", &[])?;
        let intents = [
            SecurityIntent::File {
                operation: FileIntentOperation::Read,
                path: "/tmp/base-secret".into(),
                destination: None,
            },
            SecurityIntent::File {
                operation: FileIntentOperation::Read,
                path: "/tmp/group-secret".into(),
                destination: None,
            },
            SecurityIntent::Exec {
                executable: "/usr/bin/id".into(),
                argv: vec![],
            },
            SecurityIntent::Exec {
                executable: "/usr/bin/true".into(),
                argv: vec![],
            },
            SecurityIntent::Network {
                host: "10.1.2.3".into(),
                port: Some(443),
                scheme: None,
                url: None,
            },
            SecurityIntent::Network {
                host: "2001:db8::1".into(),
                port: Some(443),
                scheme: None,
                url: None,
            },
            SecurityIntent::Network {
                host: "example.com".into(),
                port: Some(443),
                scheme: None,
                url: None,
            },
            SecurityIntent::Network {
                host: "192.0.2.1".into(),
                port: Some(22),
                scheme: None,
                url: None,
            },
        ];
        assert!(
            evaluate_intents(&bound, "worker", ENABLES, &intents)
                .iter()
                .all(|decision| !decision.allowed)
        );
        let replaced = bound.replace_group_yaml("worker", b"rules: []\n")?;
        let decisions = evaluate_intents(&replaced, "worker", ENABLES, &intents);
        assert!(!decisions[0].allowed && !decisions[2].allowed && !decisions[4].allowed);
        assert!(decisions[1].allowed && decisions[3].allowed && decisions[7].allowed);
        Ok(())
    }

    fn policy() -> Result<CompiledPolicy, PolicyError> {
        compile_yaml(
            br#"
groups:
  strict:
    rules:
      - file deny /tmp/intent-secret [read,delete]
      - exec deny /usr/bin/id
      - exec deny /usr/bin/git push
      - net deny 10.0.0.0/8
      - net allow 10.1.0.0/16:443
      - net deny example.com:443
domains:
  - name: test
    group: strict
"#,
        )
    }

    #[test]
    fn evaluates_file_and_exec_using_policy_source_semantics() -> Result<(), PolicyError> {
        let policy = policy()?;
        let decisions = evaluate_intents(
            &policy,
            "strict",
            ENABLES,
            &[
                SecurityIntent::File {
                    operation: FileIntentOperation::Read,
                    path: "/tmp/intent-secret/child".into(),
                    destination: None,
                },
                SecurityIntent::File {
                    operation: FileIntentOperation::Write,
                    path: "/tmp/intent-secret".into(),
                    destination: None,
                },
                SecurityIntent::Exec {
                    executable: "/usr/bin/id".into(),
                    argv: vec!["/usr/bin/id".into()],
                },
                SecurityIntent::Exec {
                    executable: "/usr/bin/git".into(),
                    argv: vec!["/usr/bin/git".into(), "push".into()],
                },
            ],
        );
        assert!(!decisions[0].allowed);
        assert!(decisions[1].allowed);
        assert!(!decisions[2].allowed);
        assert!(!decisions[3].allowed);
        Ok(())
    }

    #[test]
    fn network_port_rules_precede_address_rules() -> Result<(), PolicyError> {
        let policy = policy()?;
        let decisions = evaluate_intents(
            &policy,
            "strict",
            ENABLES,
            &[
                SecurityIntent::Network {
                    host: "10.1.2.3".into(),
                    port: Some(443),
                    scheme: None,
                    url: None,
                },
                SecurityIntent::Network {
                    host: "10.1.2.3".into(),
                    port: Some(22),
                    scheme: None,
                    url: None,
                },
                SecurityIntent::Network {
                    host: "example.com".into(),
                    port: Some(443),
                    scheme: None,
                    url: None,
                },
            ],
        );
        assert!(decisions[0].allowed);
        assert!(!decisions[1].allowed);
        assert!(!decisions[2].allowed);
        Ok(())
    }
}
