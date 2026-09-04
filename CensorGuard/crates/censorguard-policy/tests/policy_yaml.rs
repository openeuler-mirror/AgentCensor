use censorguard_policy::{PolicyError, compile_yaml};

#[test]
fn parses_multidomain_policy() -> Result<(), PolicyError> {
    let policy = compile_yaml(
        br#"
rules:
  - file deny+audit /etc/shadow
groups:
  standard:
    rules:
      - exec deny /usr/bin/id
      - exec deny /usr/bin/git push
      - net deny 10.0.0.0/8:22
      - net allow api.example.com:443
domains:
  - name: agent-a
    group: standard
"#,
    )?;
    assert!(policy.baseline.is_some());
    assert!(policy.has_explicit_domains);
    assert_eq!(policy.domains["agent-a"], "standard");
    assert_eq!(policy.groups["standard"].network_domains.len(), 1);
    let root = policy.to_root();
    assert_eq!(root.rules.len(), 1);
    assert_eq!(root.groups["standard"].rules.len(), 4);
    assert_eq!(root.domains.len(), 1);
    Ok(())
}

#[test]
fn unknown_yaml_field_is_rejected() {
    let result = compile_yaml(b"enable_file: true\n");
    assert!(matches!(result, Err(PolicyError::Yaml(_))));
}

#[test]
fn legacy_document_is_rejected() {
    let result = compile_yaml(
        br#"
enable_file: false
enable_exec: true
enable_net: true
allow_sample_rate: 10
policy_groups:
  standard:
    cmd_blacklist: [/usr/bin/id]
    arg_blacklist: [/usr/bin/git push]
    net_rules:
      - deny 10.0.0.0/8:22
domains:
  - name: agent-a
    group: standard
"#,
    );
    assert!(matches!(result, Err(PolicyError::Yaml(_))));
}

#[test]
fn missing_group_is_rejected() {
    let result = compile_yaml(b"domains:\n  - name: lab\n    group: missing\n");
    assert!(matches!(result, Err(PolicyError::MissingGroup { .. })));
}

#[test]
fn duplicate_domain_is_rejected() {
    let result = compile_yaml(b"domains:\n  - name: lab\n  - name: lab\n");
    assert!(matches!(result, Err(PolicyError::DuplicateDomain(_))));
}
