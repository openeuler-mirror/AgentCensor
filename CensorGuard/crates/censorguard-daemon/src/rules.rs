use censorguard_common::config::ConfigDocument;
use censorguard_policy::{PolicyRoot, compile};
use nix::unistd::{Gid, Uid, fchown};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

pub fn load(path: &Path) -> io::Result<Vec<String>> {
    let document: ConfigDocument = serde_yaml::from_slice(&fs::read(path)?).map_err(invalid)?;
    Ok(document.rules)
}

pub fn edit(path: &Path, rule: &str, append: bool) -> io::Result<()> {
    if rule.contains(['\n', '\r']) || rule.trim().is_empty() {
        return Err(invalid("expected one nonempty rule"));
    }
    let rule = normalize(rule);
    let original = fs::read(path)?;
    let metadata = fs::metadata(path)?;
    let mut document: ConfigDocument = serde_yaml::from_slice(&original).map_err(invalid)?;
    if document.daemon.dns_refresh_seconds == 0 {
        return Err(invalid("daemon.dns_refresh_seconds must be positive"));
    }
    let found = document.rules.iter().any(|value| normalize(value) == rule);
    if append {
        if !found {
            document.rules.push(rule);
        }
    } else if found {
        document.rules.retain(|value| normalize(value) != rule);
    } else {
        return Err(invalid(format!("global rule does not exist: {rule}")));
    }
    compile(PolicyRoot {
        rules: document.rules.clone(),
        daemon: document.daemon,
        ..PolicyRoot::default()
    })
    .map_err(invalid)?;
    if append && found {
        return Ok(());
    }

    let mut value: serde_yaml::Value = serde_yaml::from_slice(&original).map_err(invalid)?;
    let mapping = value
        .as_mapping_mut()
        .ok_or_else(|| invalid("startup config must be a mapping"))?;
    mapping.insert(
        serde_yaml::Value::String("rules".into()),
        serde_yaml::to_value(document.rules).map_err(invalid)?,
    );
    let yaml = serde_yaml::to_string(&value).map_err(invalid)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("configuration has no parent directory"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".censorguard-rules-")
        .tempfile_in(parent)?;
    temporary.write_all(yaml.as_bytes())?;
    let temporary_metadata = temporary.as_file().metadata()?;
    if temporary_metadata.uid() != metadata.uid() || temporary_metadata.gid() != metadata.gid() {
        fchown(
            temporary.as_file(),
            Some(Uid::from_raw(metadata.uid())),
            Some(Gid::from_raw(metadata.gid())),
        )?;
    }
    temporary
        .as_file()
        .set_permissions(metadata.permissions())?;
    temporary.as_file().sync_all()?;
    if fs::read(path)? != original {
        return Err(io::Error::other(
            "startup configuration changed while saving; retry the command",
        ));
    }
    temporary.persist(path).map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()
}

fn normalize(rule: &str) -> String {
    rule.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn edits_accumulate_without_expanding_or_changing_daemon_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("base.yaml");
        fs::write(
            &path,
            "daemon:\n  enable_exec: false\n  failure_mode: permissive\nrules:\n  - exec deny /bin/false\n",
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))?;
        let original: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path)?)?;
        let metadata = fs::metadata(&path)?;
        edit(&path, "  file   deny /tmp/secret  ", true)?;
        edit(&path, "exec deny /bin/true", true)?;
        let before_duplicate = fs::read(&path)?;
        edit(&path, "file deny /tmp/secret", true)?;
        assert_eq!(fs::read(&path)?, before_duplicate);
        edit(&path, " exec   deny /bin/false ", false)?;
        assert_eq!(
            load(&path)?,
            vec!["file deny /tmp/secret", "exec deny /bin/true"]
        );
        let saved: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path)?)?;
        assert_eq!(saved["daemon"], original["daemon"]);
        let saved_metadata = fs::metadata(&path)?;
        assert_eq!(saved_metadata.mode(), metadata.mode());
        assert_eq!(saved_metadata.uid(), metadata.uid());
        assert_eq!(saved_metadata.gid(), metadata.gid());
        Ok(())
    }

    #[test]
    fn rejected_edits_leave_configuration_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("base.yaml");
        let original = "# baseline\nrules: []\n";
        fs::write(&path, original)?;
        for rule in [
            "",
            "exec deny relative",
            "exec deny /bin/true\nexec deny /bin/false",
            "file bogus /tmp/secret",
        ] {
            assert!(edit(&path, rule, true).is_err());
            assert_eq!(fs::read_to_string(&path)?, original);
        }
        assert!(edit(&path, "exec deny /bin/true", false).is_err());
        assert_eq!(fs::read_to_string(&path)?, original);
        edit(&path, "exec deny /bin/true", true)?;
        edit(&path, "exec deny /bin/true", false)?;
        assert!(load(&path)?.is_empty());
        Ok(())
    }
}
