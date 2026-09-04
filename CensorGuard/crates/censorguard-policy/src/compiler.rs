use crate::PolicyError;
use crate::config::{GroupPolicy, PolicyRoot};
use crate::net::{DomainNetworkRule, ParsedNetworkRule, parse_network_rule};
use censorguard_common::abi::{
    ACTION_ALLOW, ACTION_DENY, ARG_INODE_MARKER, ARG_INODE_OFFSET, ARG_TOKEN_COUNT, ARG_TOKEN_LEN,
    ArgKey, CommandKey, DENY_ALL, DENY_ATTR, DENY_DELETE, DENY_READ, DENY_RENAME, DENY_WRITE,
    InodeKey, Net6LpmKey, Net6PortLpmKey, NetLpmKey, NetPortLpmKey, PATH_KEY_LEN, PathKey,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const BASE_GROUP: &str = "__base__";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuleValue {
    pub mask: u8,
    pub action: u8,
    pub audit: bool,
    pub version: u32,
}

impl RuleValue {
    fn merge(&mut self, other: Self) {
        if self.mask == 0 && self.version == 0 && !self.audit {
            self.action = other.action;
        }
        self.mask |= other.mask;
        if other.action == ACTION_DENY {
            self.action = ACTION_DENY;
        }
        self.audit |= other.audit;
        self.version = other.version;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileWarning {
    MissingPathFallback { path: String, error: String },
    MissingCommandFallback { path: String, error: String },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompiledRules {
    pub file_strings: BTreeMap<PathKey, RuleValue>,
    pub file_inodes: BTreeMap<InodeKey, RuleValue>,
    pub directory_inodes: BTreeMap<InodeKey, RuleValue>,
    pub commands: BTreeMap<CommandKey, RuleValue>,
    pub command_inodes: BTreeMap<InodeKey, RuleValue>,
    pub arguments: BTreeMap<ArgKey, RuleValue>,
    pub network: BTreeMap<NetLpmKey, RuleValue>,
    pub network_ports: BTreeMap<NetPortLpmKey, RuleValue>,
    pub network6: BTreeMap<Net6LpmKey, RuleValue>,
    pub network6_ports: BTreeMap<Net6PortLpmKey, RuleValue>,
    pub network_static: BTreeMap<NetLpmKey, RuleValue>,
    pub network_ports_static: BTreeMap<NetPortLpmKey, RuleValue>,
    pub network6_static: BTreeMap<Net6LpmKey, RuleValue>,
    pub network6_ports_static: BTreeMap<Net6PortLpmKey, RuleValue>,
    pub network_domains: Vec<DomainNetworkRule>,
}

impl CompiledRules {
    #[must_use]
    pub fn semantic_hash(&self) -> u64 {
        let mut hash = Fnv64::new();
        for (key, mask) in &self.file_strings {
            hash.field(b"file-string", &key.bytes);
            hash.bytes(&[mask.mask, mask.action, u8::from(mask.audit)]);
        }
        for (key, mask) in &self.file_inodes {
            hash.field(b"file-inode", &key.dev.to_le_bytes());
            hash.bytes(&key.ino.to_le_bytes());
            hash.bytes(&[mask.mask, mask.action, u8::from(mask.audit)]);
        }
        for (key, mask) in &self.directory_inodes {
            hash.field(b"dir-inode", &key.dev.to_le_bytes());
            hash.bytes(&key.ino.to_le_bytes());
            hash.bytes(&[mask.mask, mask.action, u8::from(mask.audit)]);
        }
        for (key, value) in &self.commands {
            hash.field(b"command", &key.bytes);
            hash.bytes(&[value.action, u8::from(value.audit)]);
        }
        for (key, value) in &self.command_inodes {
            hash.field(b"command-inode", &key.dev.to_le_bytes());
            hash.bytes(&key.ino.to_le_bytes());
            hash.bytes(&[value.action, u8::from(value.audit)]);
        }
        for (key, value) in &self.arguments {
            hash.field(b"arguments", key.tokens.as_flattened());
            hash.bytes(&[value.action, u8::from(value.audit)]);
        }
        for (key, action) in &self.network {
            hash.field(b"network", &key.prefix_len.to_le_bytes());
            hash.bytes(&key.addr.to_le_bytes());
            hash.bytes(&[action.action, u8::from(action.audit)]);
        }
        for (key, action) in &self.network_ports {
            hash.field(b"network-port", &key.prefix_len.to_le_bytes());
            hash.bytes(&key.port.to_le_bytes());
            hash.bytes(&key.addr);
            hash.bytes(&[action.action, u8::from(action.audit)]);
        }
        for (key, action) in &self.network6 {
            hash.field(b"network6", &key.prefix_len.to_le_bytes());
            hash.bytes(&key.addr);
            hash.bytes(&[action.action, u8::from(action.audit)]);
        }
        for (key, action) in &self.network6_ports {
            hash.field(b"network6-port", &key.prefix_len.to_le_bytes());
            hash.bytes(&key.port.to_le_bytes());
            hash.bytes(&key.addr);
            hash.bytes(&[action.action, u8::from(action.audit)]);
        }
        for rule in &self.network_domains {
            hash.field(b"network-domain", rule.domain.as_bytes());
            hash.byte(rule.action as u8);
            hash.bytes(&rule.port.unwrap_or_default().to_le_bytes());
            hash.byte(u8::from(rule.port.is_some()));
        }
        hash.finish()
    }
}

impl CompiledPolicy {
    #[must_use]
    pub fn to_root(&self) -> PolicyRoot {
        PolicyRoot {
            rules: self
                .baseline_definition
                .as_ref()
                .map_or_else(Vec::new, |baseline| baseline.rules.clone()),
            groups: self.group_definitions.clone(),
            domains: self
                .domains
                .iter()
                .map(|(name, group)| crate::DomainRef {
                    name: name.clone(),
                    group: group.clone(),
                })
                .collect(),
        }
    }

    pub fn replace_group_yaml(&self, name: &str, bytes: &[u8]) -> Result<Self, PolicyError> {
        if name.is_empty() {
            return Err(PolicyError::InvalidGroupName(name.to_owned()));
        }
        if name == BASE_GROUP {
            return Err(PolicyError::InvalidGroupName(format!(
                "{BASE_GROUP} must be changed through a full reload"
            )));
        }
        let group: GroupPolicy = serde_yaml::from_slice(bytes)?;
        let mut root = self.to_root();
        root.groups.insert(name.to_owned(), group);
        compile(root)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledPolicy {
    pub baseline: Option<CompiledRules>,
    pub groups: BTreeMap<String, CompiledRules>,
    pub domains: BTreeMap<String, String>,
    pub has_explicit_domains: bool,
    pub warnings: Vec<CompileWarning>,
    pub baseline_definition: Option<GroupPolicy>,
    pub group_definitions: BTreeMap<String, GroupPolicy>,
}

pub fn load(path: impl AsRef<Path>) -> Result<CompiledPolicy, PolicyError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| PolicyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    compile_yaml(&bytes)
}

pub fn compile_yaml(bytes: &[u8]) -> Result<CompiledPolicy, PolicyError> {
    let root: PolicyRoot = serde_yaml::from_slice(bytes)?;
    compile(root)
}

pub fn compile(root: PolicyRoot) -> Result<CompiledPolicy, PolicyError> {
    for name in root.groups.keys() {
        validate_name(name).map_err(|()| PolicyError::InvalidGroupName(name.clone()))?;
    }

    let has_explicit_domains = !root.domains.is_empty();
    let mut domains = BTreeMap::new();
    for domain in root.domains {
        if domain.name == BASE_GROUP || validate_name(&domain.name).is_err() {
            return Err(PolicyError::InvalidDomainName(domain.name));
        }
        if domains.contains_key(&domain.name) {
            return Err(PolicyError::DuplicateDomain(domain.name));
        }
        if !domain.group.is_empty() && !root.groups.contains_key(&domain.group) {
            return Err(PolicyError::MissingGroup {
                domain: domain.name,
                group: domain.group,
            });
        }
        domains.insert(domain.name, domain.group);
    }

    let mut warnings = Vec::new();
    let baseline_group = GroupPolicy { rules: root.rules };
    let baseline = compile_group(&baseline_group, &mut warnings)?;

    let mut compiled_groups = BTreeMap::new();
    let mut group_definitions = BTreeMap::new();
    for (name, group) in root.groups {
        let rules = compile_group(&group, &mut warnings)?;
        compiled_groups.insert(name.clone(), rules);
        group_definitions.insert(name, group);
    }

    Ok(CompiledPolicy {
        baseline: Some(baseline),
        groups: compiled_groups,
        domains,
        has_explicit_domains,
        warnings,
        baseline_definition: Some(baseline_group),
        group_definitions,
    })
}

fn compile_group(
    group: &GroupPolicy,
    warnings: &mut Vec<CompileWarning>,
) -> Result<CompiledRules, PolicyError> {
    let mut rules = CompiledRules::default();
    for line in &group.rules {
        compile_unified_rule(line, &mut rules, warnings)?;
    }
    rules.network_domains.sort();
    rules.network_domains.dedup();
    Ok(rules)
}

fn add_command_rule(
    value: &str,
    action: u8,
    audit: bool,
    rules: &mut CompiledRules,
    warnings: &mut Vec<CompileWarning>,
) -> Result<(), PolicyError> {
    let key = pack_absolute_path("command", value)?;
    rules.commands.entry(key).or_default().merge(RuleValue {
        mask: 1,
        action,
        audit,
        version: 0,
    });
    match executable_inode(value) {
        Ok(inode) => {
            rules
                .command_inodes
                .entry(inode)
                .or_default()
                .merge(RuleValue {
                    mask: 1,
                    action,
                    audit,
                    version: 0,
                });
        }
        Err(error) => {
            warnings.push(CompileWarning::MissingCommandFallback {
                path: value.to_owned(),
                error: error.to_string(),
            });
        }
    }
    Ok(())
}

fn add_argument_rule(
    value: &str,
    action: u8,
    audit: bool,
    rules: &mut CompiledRules,
    warnings: &mut Vec<CompileWarning>,
) -> Result<(), PolicyError> {
    let executable = value
        .split_whitespace()
        .next()
        .ok_or_else(|| PolicyError::ArgTokenCount {
            rule: value.to_owned(),
            maximum: ARG_TOKEN_COUNT,
        })?;
    let mut key = pack_arguments(value)?;
    rules.arguments.entry(key).or_default().merge(RuleValue {
        mask: 1,
        action,
        audit,
        version: 0,
    });
    match executable_inode(executable) {
        Ok(inode) => {
            encode_argument_inode(&mut key, inode);
            rules.arguments.entry(key).or_default().merge(RuleValue {
                mask: 1,
                action,
                audit,
                version: 0,
            });
        }
        Err(error) => warnings.push(CompileWarning::MissingCommandFallback {
            path: executable.to_owned(),
            error: error.to_string(),
        }),
    }
    Ok(())
}

fn executable_inode(value: &str) -> Result<InodeKey, std::io::Error> {
    fs::metadata(value).map(|metadata| InodeKey {
        dev: kernel_dev(metadata.dev()),
        ino: metadata.ino(),
    })
}

fn encode_argument_inode(key: &mut ArgKey, inode: InodeKey) {
    key.tokens[0] = [0; ARG_TOKEN_LEN];
    key.tokens[0][0] = ARG_INODE_MARKER;
    key.tokens[0][ARG_INODE_OFFSET..ARG_INODE_OFFSET + 8].copy_from_slice(&inode.dev.to_le_bytes());
    key.tokens[0][ARG_INODE_OFFSET + 8..ARG_INODE_OFFSET + 16]
        .copy_from_slice(&inode.ino.to_le_bytes());
}

fn add_file_rule(
    value: &str,
    mask: u8,
    action: u8,
    audit: bool,
    rules: &mut CompiledRules,
    warnings: &mut Vec<CompileWarning>,
) -> Result<(), PolicyError> {
    let key = pack_absolute_path("file rule", value)?;
    match fs::metadata(value) {
        Ok(metadata) => {
            let inode = InodeKey {
                dev: kernel_dev(metadata.dev()),
                ino: metadata.ino(),
            };
            rules
                .file_inodes
                .entry(inode)
                .or_default()
                .merge(RuleValue {
                    mask,
                    action,
                    audit,
                    version: 0,
                });
            if metadata.is_dir() {
                rules
                    .directory_inodes
                    .entry(inode)
                    .or_default()
                    .merge(RuleValue {
                        mask,
                        action,
                        audit,
                        version: 0,
                    });
            }
        }
        Err(error) => {
            rules.file_strings.entry(key).or_default().merge(RuleValue {
                mask,
                action,
                audit,
                version: 0,
            });
            warnings.push(CompileWarning::MissingPathFallback {
                path: value.to_owned(),
                error: error.to_string(),
            });
        }
    }
    Ok(())
}

fn compile_unified_rule(
    line: &str,
    rules: &mut CompiledRules,
    warnings: &mut Vec<CompileWarning>,
) -> Result<(), PolicyError> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 3 {
        return Err(PolicyError::Network {
            rule: line.to_owned(),
            reason: "rule requires TYPE ACTION TARGET".into(),
        });
    }
    let action_token = fields[1].to_ascii_lowercase();
    let (action, audit) = match action_token.as_str() {
        "deny" => (ACTION_DENY, false),
        "allow" => (ACTION_ALLOW, false),
        "deny+audit" => (ACTION_DENY, true),
        "allow+audit" => (ACTION_ALLOW, true),
        _ => {
            return Err(PolicyError::Network {
                rule: line.to_owned(),
                reason: "action must be allow, deny, allow+audit or deny+audit".into(),
            });
        }
    };
    match fields[0] {
        "file" => {
            let mask = match fields.get(3).copied().unwrap_or("none") {
                "none" => DENY_ALL,
                "ro" | "read_only" => DENY_WRITE | DENY_DELETE | DENY_RENAME | DENY_ATTR,
                "rw" | "read_write" => DENY_DELETE | DENY_RENAME | DENY_ATTR,
                "noattr" => DENY_ATTR,
                level if level.starts_with('[') && level.ends_with(']') => {
                    let ops: Vec<String> = level[1..level.len() - 1]
                        .split(',')
                        .map(|s| s.to_owned())
                        .collect();
                    file_mask(fields[2], &ops)?
                }
                other => {
                    return Err(PolicyError::UnknownFileOperation {
                        path: fields[2].to_owned(),
                        operation: other.to_owned(),
                    });
                }
            };
            add_file_rule(fields[2], mask, action, audit, rules, warnings)
        }
        "exec" => {
            add_argument_rule(&fields[2..].join(" "), action, audit, rules, warnings)?;
            if fields.len() == 3 {
                add_command_rule(fields[2], action, audit, rules, warnings)?;
            }
            Ok(())
        }
        "net" => {
            let parsed = parse_network_rule(&format!(
                "{} {}",
                if action == ACTION_ALLOW {
                    "allow"
                } else {
                    "deny"
                },
                fields[2..].join(" ")
            ))?;
            match parsed {
                ParsedNetworkRule::Address4 { key, .. } => {
                    rules.network.entry(key).or_default().merge(RuleValue {
                        mask: 0,
                        action,
                        audit,
                        version: 0,
                    });
                }
                ParsedNetworkRule::Address4Port { key, .. } => {
                    rules
                        .network_ports
                        .entry(key)
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action,
                            audit,
                            version: 0,
                        });
                }
                ParsedNetworkRule::Address6 { key, .. } => {
                    rules.network6.entry(key).or_default().merge(RuleValue {
                        mask: 0,
                        action,
                        audit,
                        version: 0,
                    });
                }
                ParsedNetworkRule::Address6Port { key, .. } => {
                    rules
                        .network6_ports
                        .entry(key)
                        .or_default()
                        .merge(RuleValue {
                            mask: 0,
                            action,
                            audit,
                            version: 0,
                        });
                }
                ParsedNetworkRule::Domain(mut domain) => {
                    domain.audit = audit;
                    rules.network_domains.push(domain);
                }
            }
            Ok(())
        }
        other => Err(PolicyError::Network {
            rule: line.to_owned(),
            reason: format!("unknown rule type {other}"),
        }),
    }
}

fn kernel_dev(encoded: u64) -> u64 {
    let major = (encoded >> 8) & 0x0fff;
    let minor = (encoded & 0x00ff) | ((encoded >> 12) & !0x00ff);
    (major << 20) | minor
}

fn file_mask(path: &str, operations: &[String]) -> Result<u8, PolicyError> {
    if operations.is_empty() {
        return Err(PolicyError::EmptyFileDeny {
            path: path.to_owned(),
        });
    }
    operations.iter().try_fold(0, |mask, operation| {
        let bit = match operation.trim().to_ascii_lowercase().as_str() {
            "read" => DENY_READ,
            "write" => DENY_WRITE,
            "delete" => DENY_DELETE,
            "rename" => DENY_RENAME,
            "attr" => DENY_ATTR,
            _ => {
                return Err(PolicyError::UnknownFileOperation {
                    path: path.to_owned(),
                    operation: operation.clone(),
                });
            }
        };
        Ok(mask | bit)
    })
}

fn pack_absolute_path(kind: &'static str, value: &str) -> Result<PathKey, PolicyError> {
    if !PathBuf::from(value).is_absolute() {
        return Err(PolicyError::RelativePath {
            kind,
            value: value.to_owned(),
        });
    }
    pack_path(kind, value)
}

fn pack_path(kind: &'static str, value: &str) -> Result<PathKey, PolicyError> {
    let bytes = value.as_bytes();
    if bytes.contains(&0) {
        return Err(PolicyError::EmbeddedNul {
            kind,
            value: value.to_owned(),
        });
    }
    if bytes.len() >= PATH_KEY_LEN {
        return Err(PolicyError::KeyTooLong {
            kind,
            value: value.to_owned(),
            actual: bytes.len(),
            maximum: PATH_KEY_LEN - 1,
        });
    }
    let mut key = PathKey::default();
    key.bytes[..bytes.len()].copy_from_slice(bytes);
    Ok(key)
}

fn pack_arguments(value: &str) -> Result<ArgKey, PolicyError> {
    let tokens: Vec<_> = value.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > ARG_TOKEN_COUNT {
        return Err(PolicyError::ArgTokenCount {
            rule: value.to_owned(),
            maximum: ARG_TOKEN_COUNT,
        });
    }
    if !Path::new(tokens[0]).is_absolute() {
        return Err(PolicyError::RelativePath {
            kind: "argument rule executable",
            value: tokens[0].to_owned(),
        });
    }
    let mut key = ArgKey::default();
    for (index, token) in tokens.iter().enumerate() {
        if token.as_bytes().contains(&0) {
            return Err(PolicyError::EmbeddedNul {
                kind: "argument token",
                value: (*token).to_owned(),
            });
        }
        if token.len() >= ARG_TOKEN_LEN {
            return Err(PolicyError::ArgTokenTooLong {
                token: (*token).to_owned(),
                actual: token.len(),
                maximum: ARG_TOKEN_LEN - 1,
            });
        }
        key.tokens[index][..token.len()].copy_from_slice(token.as_bytes());
    }
    Ok(key)
}

fn validate_name(value: &str) -> Result<(), ()> {
    if value.is_empty() || value.len() > 63 {
        return Err(());
    }
    if value
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err(())
    }
}

struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 14_695_981_039_346_656_037;
    const PRIME: u64 = 1_099_511_628_211;

    const fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn field(&mut self, name: &[u8], value: &[u8]) {
        self.bytes(&(name.len() as u64).to_le_bytes());
        self.bytes(name);
        self.bytes(&(value.len() as u64).to_le_bytes());
        self.bytes(value);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_rules_are_compiled_as_global_baseline() -> Result<(), PolicyError> {
        let policy =
            compile_yaml(b"rules:\n  - file deny+audit /etc/shadow\n  - exec deny /usr/bin/nc\n")?;
        assert!(policy.baseline.is_some());
        assert!(policy.groups.is_empty());
        assert_eq!(
            policy.baseline_definition.as_ref().map(|g| g.rules.len()),
            Some(2)
        );
        assert_eq!(policy.to_root().rules.len(), 2);
        Ok(())
    }
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn compiles_file_and_directory_to_inode_keys() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let file = directory.path().join("protected.txt");
        File::create(&file)?;
        let group = GroupPolicy {
            rules: vec![format!(
                "file deny {} [write,delete]",
                file.to_string_lossy()
            )],
        };
        let mut root = PolicyRoot::default();
        root.groups.insert("lab".into(), group);
        let compiled = compile(root)?;
        let rules = &compiled.groups["lab"];
        assert_eq!(rules.file_inodes.len(), 1);
        assert!(rules.file_strings.is_empty());
        Ok(())
    }

    #[test]
    fn rejects_silent_argument_truncation() {
        let result = pack_arguments("/usr/bin/git push origin main extra");
        assert!(matches!(
            result,
            Err(PolicyError::ArgTokenCount { maximum: 4, .. })
        ));
    }

    #[test]
    fn semantic_hash_is_insertion_order_independent() -> Result<(), PolicyError> {
        let mut left = CompiledRules::default();
        left.commands.insert(
            pack_absolute_path("command", "/usr/bin/id")?,
            RuleValue {
                mask: 1,
                action: ACTION_DENY,
                audit: false,
                version: 0,
            },
        );
        left.commands.insert(
            pack_absolute_path("command", "/usr/bin/git")?,
            RuleValue {
                mask: 1,
                action: ACTION_DENY,
                audit: false,
                version: 0,
            },
        );
        let mut right = CompiledRules::default();
        right.commands.insert(
            pack_absolute_path("command", "/usr/bin/git")?,
            RuleValue {
                mask: 1,
                action: ACTION_DENY,
                audit: false,
                version: 0,
            },
        );
        right.commands.insert(
            pack_absolute_path("command", "/usr/bin/id")?,
            RuleValue {
                mask: 1,
                action: ACTION_DENY,
                audit: false,
                version: 0,
            },
        );
        assert_eq!(left.semantic_hash(), right.semantic_hash());
        Ok(())
    }

    #[test]
    fn compiles_command_and_arguments_to_executable_inode() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempdir()?;
        let executable = directory.path().join("tool");
        File::create(&executable)?;
        let executable = executable.to_string_lossy().into_owned();
        let yaml = format!(
            "groups:\n  lab:\n    rules:\n      - exec deny {executable}\n      - exec deny {executable} blocked\n"
        );
        let compiled = compile_yaml(yaml.as_bytes())?;
        let rules = &compiled.groups["lab"];
        assert_eq!(rules.command_inodes.len(), 1);
        assert_eq!(rules.commands.len(), 1);
        assert_eq!(rules.arguments.len(), 4);
        let arguments: Vec<_> = rules.arguments.iter().collect();
        assert!(
            arguments
                .iter()
                .any(|argument| argument.0.tokens[0][0] == ARG_INODE_MARKER)
        );
        Ok(())
    }

    #[test]
    fn unified_rules_merge_deny_over_allow_and_preserve_audit() -> Result<(), PolicyError> {
        let policy = compile_yaml(
            br#"
groups:
  test:
    rules:
      - file allow+audit /tmp/censorguard-rule
      - file deny /tmp/censorguard-rule [read]
      - exec allow+audit /usr/bin/id
      - exec deny /usr/bin/id
      - net allow+audit 192.0.2.1:443
"#,
        )?;
        let rules = &policy.groups["test"];
        let file = rules
            .file_strings
            .values()
            .next()
            .ok_or(PolicyError::EmptyFileDeny {
                path: "missing".into(),
            })?;
        assert_eq!(file.action, ACTION_DENY);
        assert!(file.audit);
        let cmd = rules
            .commands
            .values()
            .next()
            .ok_or(PolicyError::InvalidGroupName("missing command".into()))?;
        assert_eq!(cmd.action, ACTION_DENY);
        assert!(cmd.audit);
        let net = rules
            .network_ports
            .values()
            .next()
            .ok_or(PolicyError::InvalidGroupName("missing network".into()))?;
        assert_eq!(net.action, ACTION_ALLOW);
        assert!(net.audit);
        Ok(())
    }

    #[test]
    fn legacy_fields_are_rejected_as_unknown() {
        assert!(matches!(
            compile_yaml(b"enable_file: true\n"),
            Err(PolicyError::Yaml(_))
        ));
        assert!(matches!(
            compile_yaml(b"policy_groups:\n  lab:\n    cmd_blacklist: [/usr/bin/id]\n"),
            Err(PolicyError::Yaml(_))
        ));
    }
}
