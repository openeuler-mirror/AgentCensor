//! Passive TLS target discovery and coverage bookkeeping.
//!
//! This module deliberately stops before attachment: a target is never marked
//! covered until a later uprobe attach operation reports success.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TlsLibrary {
    OpenSsl,
    GnuTls,
    Nss,
    GoTls,
    Rustls,
}

impl TlsLibrary {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenSsl => "openssl",
            Self::GnuTls => "gnutls",
            Self::Nss => "nss",
            Self::GoTls => "go_tls",
            Self::Rustls => "rustls",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsImage {
    pub path: PathBuf,
    pub library: TlsLibrary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageState {
    Discovered,
    Covered,
    ResolveFailure,
    AttachFailure,
    EarlyWindowGap,
}

impl CoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Covered => "covered",
            Self::ResolveFailure => "resolve_failure",
            Self::AttachFailure => "attach_failure",
            Self::EarlyWindowGap => "early_window_gap",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsCoverageRecord {
    pub pid: u32,
    pub generation: u64,
    pub image: Option<TlsImage>,
    pub state: CoverageState,
    pub message: String,
}

impl TlsCoverageRecord {
    /// Mark coverage only after the kernel confirms uprobe attachment.
    pub fn attached(mut self) -> Self {
        self.state = CoverageState::Covered;
        self.message = "TLS plaintext coverage starts at successful uprobe attachment".to_string();
        self
    }

    pub fn attach_failed(mut self, message: impl Into<String>) -> Self {
        self.state = CoverageState::AttachFailure;
        self.message = message.into();
        self
    }

    pub fn resolve_probe_plan(mut self) -> (Self, Option<Vec<crate::tls_resolver::TlsProbePoint>>) {
        let Some(image) = &self.image else {
            return (self, None);
        };
        match crate::tls_resolver::resolve_library_plan(&image.path, image.library) {
            Ok(points) => (self, Some(points)),
            Err(message) => {
                self.state = CoverageState::ResolveFailure;
                self.message = message;
                (self, None)
            }
        }
    }
}

/// Read loaded executable/shared-library paths for one process and retain only
/// known TLS implementations. Duplicate mappings of one image are collapsed.
pub fn discover_process_images(pid: u32) -> Result<Vec<TlsImage>, String> {
    let path = format!("/proc/{pid}/maps");
    let contents = std::fs::read_to_string(&path).map_err(|error| format!("{path}: {error}"))?;
    let mut seen = BTreeSet::new();
    let mut images = Vec::new();
    for line in contents.lines() {
        let Some(image_path) = line.split_whitespace().nth(5) else {
            continue;
        };
        let image_path = image_path.strip_suffix(" (deleted)").unwrap_or(image_path);
        let image_path = Path::new(image_path);
        let Some(library) = classify_tls_image(image_path) else {
            continue;
        };
        let image_path = image_path.to_path_buf();
        if seen.insert(image_path.clone()) {
            images.push(TlsImage {
                path: image_path,
                library,
            });
        }
    }
    Ok(images)
}

pub fn classify_tls_image(path: &Path) -> Option<TlsLibrary> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.starts_with("libssl") || name.contains("boringssl") {
        Some(TlsLibrary::OpenSsl)
    } else if name.starts_with("libgnutls") {
        Some(TlsLibrary::GnuTls)
    } else if name.starts_with("libnspr") || name.starts_with("libnss") {
        Some(TlsLibrary::Nss)
    } else {
        // Go binaries are usually statically linked with no distinctive
        // filename; the .gopclntab marker is the passive discovery signal.
        let bytes = std::fs::read(path).ok()?;
        if bytes
            .windows(b".gopclntab".len())
            .any(|window| window == b".gopclntab")
        {
            Some(TlsLibrary::GoTls)
        } else if bytes
            .windows(b"rustls".len())
            .any(|window| window == b"rustls")
        {
            Some(TlsLibrary::Rustls)
        } else if crate::tls_resolver::resolve_library_plan(path, TlsLibrary::OpenSsl).is_ok() {
            // Node.js embeds BoringSSL directly in the node executable: no
            // libssl.so filename, but exported SSL_read/SSL_write symbols
            // remain, so it matches the OpenSSL probe plan.
            Some(TlsLibrary::OpenSsl)
        } else {
            None
        }
    }
}

pub fn discovery_records(pid: u32, generation: u64) -> Vec<TlsCoverageRecord> {
    match discover_process_images(pid) {
        Ok(images) => discovery_records_from_images(pid, generation, images),
        Err(message) => vec![TlsCoverageRecord {
            pid,
            generation,
            image: None,
            state: CoverageState::ResolveFailure,
            message,
        }],
    }
}

pub fn discovery_records_from_images(
    pid: u32,
    generation: u64,
    images: Vec<TlsImage>,
) -> Vec<TlsCoverageRecord> {
    if images.is_empty() {
        return vec![TlsCoverageRecord {
            pid,
            generation,
            image: None,
            state: CoverageState::EarlyWindowGap,
            message: "no recognized TLS image was loaded; plaintext coverage has not started"
                .to_string(),
        }];
    }
    images
        .into_iter()
        .map(|image| TlsCoverageRecord {
            pid,
            generation,
            image: Some(image),
            state: CoverageState::Discovered,
            message: "TLS target resolved; uprobe attachment is still pending".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_supported_tls_images_only() {
        assert_eq!(
            classify_tls_image(Path::new("/usr/lib/libssl.so.3")),
            Some(TlsLibrary::OpenSsl)
        );
        assert_eq!(
            classify_tls_image(Path::new("/usr/lib/libgnutls.so.30")),
            Some(TlsLibrary::GnuTls)
        );
        assert_eq!(
            classify_tls_image(Path::new("/usr/lib/libnspr4.so")),
            Some(TlsLibrary::Nss)
        );
        assert_eq!(classify_tls_image(Path::new("/tmp/agent")), None);
    }

    #[test]
    fn classifies_static_node_openssl_when_available() {
        let path = Path::new("/home/dev/.nvm/versions/node/v22.19.0/bin/node");
        if !path.exists() {
            return;
        }
        assert_eq!(classify_tls_image(path), Some(TlsLibrary::OpenSsl));
    }

    #[test]
    fn discovery_for_current_process_never_claims_coverage() {
        let records = discovery_records(std::process::id(), 1);
        assert!(!records.is_empty());
        assert!(records.iter().all(|record| {
            matches!(
                record.state,
                CoverageState::Discovered | CoverageState::EarlyWindowGap
            )
        }));
        assert!(
            records
                .iter()
                .all(|record| record.state != CoverageState::Covered)
        );
    }

    #[test]
    fn coverage_starts_only_after_explicit_attach_success() {
        let record = TlsCoverageRecord {
            pid: 1,
            generation: 2,
            image: Some(TlsImage {
                path: PathBuf::from("/lib/libssl.so"),
                library: TlsLibrary::OpenSsl,
            }),
            state: CoverageState::Discovered,
            message: "pending".to_string(),
        };
        assert_eq!(
            record.clone().attach_failed("permission").state,
            CoverageState::AttachFailure
        );
        assert_eq!(record.attached().state, CoverageState::Covered);
    }
}
