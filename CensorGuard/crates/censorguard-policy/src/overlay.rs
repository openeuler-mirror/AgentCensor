//! Compose independently evaluated group layers into a kernel rule slot.
//!
//! File/exec denies are unions. Network maps encode the union of each layer's
//! port-first, longest-prefix decision, so an allow cannot hide another layer's deny.
use crate::compiler::{CompiledRules, RuleValue};
use censorguard_common::abi::{Net6LpmKey, Net6PortLpmKey, NetLpmKey, NetPortLpmKey};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
struct PrefixRule {
    address: u128,
    prefix: u32,
    port: Option<u16>,
    value: RuleValue,
}

fn mask(address: u128, prefix: u32) -> u128 {
    if prefix == 0 {
        0
    } else {
        address & (u128::MAX << (128 - prefix))
    }
}

fn select(
    layer: &[PrefixRule],
    address: u128,
    prefix: u32,
    port: Option<u16>,
) -> Option<RuleValue> {
    let lookup = |target_port| {
        layer
            .iter()
            .filter(|rule| {
                rule.port == target_port
                    && rule.prefix <= prefix
                    && mask(address, rule.prefix) == rule.address
            })
            .max_by_key(|rule| rule.prefix)
            .map(|rule| rule.value)
    };
    port.and_then(|value| lookup(Some(value)))
        .or_else(|| lookup(None))
}

fn network(layers: &[Vec<PrefixRule>]) -> BTreeMap<(u128, u32, Option<u16>), RuleValue> {
    let mut boundaries = BTreeSet::from([(0, 0)]);
    let mut ports = BTreeSet::from([None]);
    for layer in layers {
        for rule in layer {
            boundaries.insert((rule.address, rule.prefix));
            if rule.port.is_some() {
                ports.insert(rule.port);
            }
        }
    }
    let mut result = BTreeMap::new();
    for port in ports {
        for &(address, prefix) in &boundaries {
            let mut combined = RuleValue::default();
            let mut matched = false;
            for layer in layers {
                if let Some(value) = select(layer, address, prefix, port) {
                    combined.merge(value);
                    matched = true;
                }
            }
            // A port map must override the no-port union even where its decision is allow.
            if matched || port.is_some() {
                result.insert((address, prefix, port), combined);
            }
        }
    }
    result
}

pub(crate) fn compose(layers: &[&CompiledRules]) -> CompiledRules {
    let mut result = CompiledRules::default();
    macro_rules! merge_maps {
        ($($field:ident),+) => { $(
            for layer in layers {
                for (key, value) in &layer.$field {
                    result.$field.entry(*key).or_default().merge(*value);
                }
            }
        )+ };
    }
    merge_maps!(
        file_strings,
        file_inodes,
        directory_inodes,
        commands,
        command_inodes,
        arguments
    );
    let ipv4: Vec<Vec<_>> = layers
        .iter()
        .map(|layer| {
            layer
                .network
                .iter()
                .map(|(key, value)| PrefixRule {
                    address: mask(
                        u128::from(u32::from_be_bytes(key.addr.to_le_bytes())) << 96,
                        key.prefix_len,
                    ),
                    prefix: key.prefix_len,
                    port: None,
                    value: *value,
                })
                .chain(layer.network_ports.iter().map(|(key, value)| PrefixRule {
                    address: mask(
                        u128::from(u32::from_be_bytes(key.addr)) << 96,
                        key.prefix_len - 16,
                    ),
                    prefix: key.prefix_len - 16,
                    port: Some(key.port),
                    value: *value,
                }))
                .collect()
        })
        .collect();
    for ((address, prefix, port), value) in network(&ipv4) {
        let bytes = ((address >> 96) as u32).to_be_bytes();
        if let Some(port) = port {
            result.network_ports.insert(
                NetPortLpmKey {
                    prefix_len: prefix + 16,
                    port,
                    addr: bytes,
                    pad: 0,
                },
                value,
            );
        } else {
            result.network.insert(
                NetLpmKey {
                    prefix_len: prefix,
                    addr: u32::from_le_bytes(bytes),
                    port: 0,
                    pad: 0,
                },
                value,
            );
        }
    }
    let ipv6: Vec<Vec<_>> = layers
        .iter()
        .map(|layer| {
            layer
                .network6
                .iter()
                .map(|(key, value)| PrefixRule {
                    address: mask(u128::from_be_bytes(key.addr), key.prefix_len),
                    prefix: key.prefix_len,
                    port: None,
                    value: *value,
                })
                .chain(layer.network6_ports.iter().map(|(key, value)| PrefixRule {
                    address: mask(u128::from_be_bytes(key.addr), key.prefix_len - 16),
                    prefix: key.prefix_len - 16,
                    port: Some(key.port),
                    value: *value,
                }))
                .collect()
        })
        .collect();
    for ((address, prefix, port), value) in network(&ipv6) {
        if let Some(port) = port {
            result.network6_ports.insert(
                Net6PortLpmKey {
                    prefix_len: prefix + 16,
                    port,
                    addr: address.to_be_bytes(),
                    pad: 0,
                },
                value,
            );
        } else {
            result.network6.insert(
                Net6LpmKey {
                    prefix_len: prefix,
                    addr: address.to_be_bytes(),
                },
                value,
            );
        }
    }
    result.network_static = result.network.clone();
    result.network_ports_static = result.network_ports.clone();
    result.network6_static = result.network6.clone();
    result.network6_ports_static = result.network6_ports.clone();
    result
}
