//! Container identity resolved from a process's cgroup.
//!
//! `container_id` is the runtime-assigned, human-readable, stable handle for a
//! container. It is 1:1 with the container's pid namespace, but unlike the
//! kernel `NamespaceIdentity` (an opaque, reuse-prone inode) it maps to
//! `docker ps` / image / pod and survives collector restarts.
//!
//! The resolver supports Docker, containerd/Kata and Kubernetes cgroup layouts.

/// Which container runtime a [`ContainerIdentity`] was parsed from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContainerRuntime {
    Docker,
    Containerd,
    K8s,
    Unknown,
}

/// Readable, stable runtime-assigned container identity associated with a process.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContainerIdentity {
    pub runtime: ContainerRuntime,
    pub container_id: String,
}

impl ContainerIdentity {
    pub fn new(runtime: ContainerRuntime, container_id: impl Into<String>) -> Self {
        Self {
            runtime,
            container_id: container_id.into(),
        }
    }

    pub fn container_id(&self) -> &str {
        &self.container_id
    }
}
