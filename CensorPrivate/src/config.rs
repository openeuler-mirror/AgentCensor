//! Non-secret deployment configuration. Paths resolve against the YAML file.
use crate::{
    categories::{self, Categories},
    error::{Error, Result},
    policy::{CustomSettings, PrivacyPolicy},
    professional::{
        configured_ner_batch_size, configured_ner_device, configured_ner_threads, NerDevice,
        ProfessionalConfig,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Read,
    net::IpAddr,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

fn read_yaml(path: &Path) -> Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| Error::ConfigInvalid)?;
    if !file.metadata().map_err(|_| Error::ConfigInvalid)?.is_file() {
        return Err(Error::ConfigInvalid);
    }
    let mut raw = String::new();
    file.take(1_048_577)
        .read_to_string(&mut raw)
        .map_err(|_| Error::ConfigInvalid)?;
    if raw.len() > 1_048_576 {
        return Err(Error::ConfigInvalid);
    }
    Ok(raw)
}

/// Compatibility reader for the former standalone policy CLI option.
pub fn load_policy(path: Option<&Path>) -> Result<PrivacyPolicy> {
    match path {
        Some(path) => PrivacyPolicy::from_yaml(&read_yaml(path)?),
        None => Ok(PrivacyPolicy::default()),
    }
}

#[derive(clap::Args, Default)]
pub struct ConfigArgs {
    /// Deployment YAML; otherwise CP_CONFIG_FILE, then ./config.yaml.
    #[arg(long, conflicts_with = "no_config")]
    pub config: Option<PathBuf>,
    /// Ignore automatic deployment configuration discovery.
    #[arg(long)]
    pub no_config: bool,
}
impl ConfigArgs {
    pub fn load(&self) -> Result<Option<AppConfig>> {
        self.path()?.as_deref().map(AppConfig::load).transpose()
    }
    pub fn path(&self) -> Result<Option<PathBuf>> {
        if self.no_config {
            return Ok(None);
        }
        let explicit = self
            .config
            .clone()
            .or_else(|| std::env::var_os("CP_CONFIG_FILE").map(PathBuf::from));
        let path = match explicit {
            Some(path) => path,
            None => {
                let path = PathBuf::from("config.yaml");
                if !path.try_exists().map_err(|_| Error::ConfigInvalid)? {
                    return Ok(None);
                }
                path
            }
        };
        Ok(Some(
            std::env::current_dir()
                .map_err(|_| Error::ConfigInvalid)?
                .join(path),
        ))
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub server: ServerSettings,
    pub detection: DetectionSettings,
    pub ner: NerSettings,
    pub custom: CustomSettings,
    pub limits: LimitSettings,
    pub private_tools_file: Option<PathBuf>,
    pub conversation_records_dir: Option<PathBuf>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    pub host: Option<IpAddr>,
    pub port: Option<u16>,
    pub ttl_seconds: Option<f64>,
    pub upstream: Option<String>,
    pub models: Option<Vec<String>>,
    pub token_file: Option<PathBuf>,
    pub upstream_key_file: Option<PathBuf>,
    pub upstream_ca_file: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_direct: Option<bool>,
    pub allow_http_test_upstream: Option<bool>,
    pub run_state_file: Option<PathBuf>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DetectionSettings {
    pub categories: Option<CategorySelection>,
    pub sensitive_paths: Option<Vec<String>>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CategorySelection {
    Keyword(String),
    List(Vec<String>),
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NerSettings {
    pub threads: Option<u16>,
    pub device: Option<String>,
    pub batch_size: Option<u16>,
    pub window_timeout_seconds: Option<u64>,
    pub model_dir: Option<PathBuf>,
    pub ort_library: Option<PathBuf>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitSettings {
    pub max_body_bytes: Option<usize>,
    pub max_decoded_bytes: Option<usize>,
    pub upstream_timeout_seconds: Option<u64>,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        // Kubernetes projected volumes intentionally use symlinks; follow the
        // ConfigMap path, read one bounded snapshot and never write it back.
        let mut config = Self::from_yaml(&read_yaml(path)?)?;
        let parent = std::env::current_dir()
            .map_err(|_| Error::ConfigInvalid)?
            .join(path)
            .parent()
            .ok_or(Error::ConfigInvalid)?
            .to_path_buf();
        let server = &mut config.server;
        for value in [
            &mut server.token_file,
            &mut server.upstream_key_file,
            &mut server.upstream_ca_file,
            &mut server.tls_cert,
            &mut server.tls_key,
            &mut server.run_state_file,
            &mut config.ner.model_dir,
            &mut config.ner.ort_library,
            &mut config.private_tools_file,
            &mut config.conversation_records_dir,
        ]
        .into_iter()
        .flatten()
        {
            if value.is_relative() {
                *value = parent.join(&*value);
            }
        }
        Ok(config)
    }
    pub fn from_yaml(raw: &str) -> Result<Self> {
        let config: Self = serde_yaml::from_str(raw).map_err(|_| Error::ConfigInvalid)?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        if self.server.port == Some(0)
            || self
                .server
                .models
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.iter().any(|m| m.trim().is_empty()))
            || [self.ner.threads, self.ner.batch_size]
                .iter()
                .flatten()
                .any(|n| !(1..=256).contains(n))
            || self
                .ner
                .window_timeout_seconds
                .is_some_and(|n| !(1..=3600).contains(&n))
            || self
                .limits
                .upstream_timeout_seconds
                .is_some_and(|n| !(1..=86400).contains(&n))
            || [self.limits.max_body_bytes, self.limits.max_decoded_bytes]
                .iter()
                .flatten()
                .any(|n| !(1..=268_435_456).contains(n))
        {
            return Err(Error::ConfigInvalid);
        }
        if [
            &self.server.token_file,
            &self.server.upstream_key_file,
            &self.server.upstream_ca_file,
            &self.server.tls_cert,
            &self.server.tls_key,
            &self.server.run_state_file,
            &self.ner.model_dir,
            &self.ner.ort_library,
            &self.private_tools_file,
            &self.conversation_records_dir,
        ]
        .into_iter()
        .flatten()
        .any(|p| p.as_os_str().is_empty())
        {
            return Err(Error::ConfigInvalid);
        }
        if let Some(device) = &self.ner.device {
            device
                .parse::<NerDevice>()
                .map_err(|_| Error::ConfigInvalid)?;
        }
        self.privacy_policy().map_err(|_| Error::ConfigInvalid)?;
        Ok(())
    }
    pub fn categories(&self) -> Result<Categories> {
        match &self.detection.categories {
            None => Ok(Some(Default::default())),
            Some(CategorySelection::Keyword(keyword)) if keyword == "all" => Ok(None),
            Some(CategorySelection::Keyword(_)) => Err(Error::ConfigInvalid),
            Some(CategorySelection::List(items)) => Ok(Some(categories::normalize(items)?)),
        }
    }
    pub fn apply_policy(&self, policy: PrivacyPolicy) -> Result<PrivacyPolicy> {
        let policy = policy.with_deployment(self.server.ttl_seconds, self.categories()?)?;
        if policy.ttl().as_secs_f64() > 315_360_000.0 {
            return Err(Error::ConfigInvalid);
        }
        Ok(policy)
    }
    pub fn privacy_policy(&self) -> Result<PrivacyPolicy> {
        self.apply_policy(PrivacyPolicy::from_custom(self.custom.clone())?)
    }
    pub fn detector(
        &self,
        threads: Option<u16>,
        device: Option<NerDevice>,
        batch: Option<u16>,
        model: Option<PathBuf>,
        library: Option<PathBuf>,
    ) -> Result<ProfessionalConfig> {
        Ok(ProfessionalConfig {
            ner_threads: configured_ner_threads(threads.or(self.ner.threads))?,
            ner_device: configured_ner_device(
                device.or(self
                    .ner
                    .device
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| Error::ConfigInvalid)?),
            )?,
            ner_batch_size: configured_ner_batch_size(batch.or(self.ner.batch_size))?,
            window_timeout_seconds: self.ner.window_timeout_seconds.unwrap_or(16),
            categories: self.categories()?.or_else(|| {
                Some(
                    categories::BUILTIN_CATEGORIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                )
            }),
            model_directory: model
                .or_else(|| self.ner.model_dir.clone())
                .or_else(|| ProfessionalConfig::default().model_directory),
            runtime_library: library
                .or_else(|| self.ner.ort_library.clone())
                .or_else(|| ProfessionalConfig::default().runtime_library),
        })
    }
}
