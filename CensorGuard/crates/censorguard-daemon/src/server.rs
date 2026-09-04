use crate::event::EventHub;
use crate::runtime::{Runtime, RuntimeError};
use censorguard_common::protocol::{
    Operation, Request, Response, RpcError, RpcErrorCode, RpcMethod, RpcRequest, RpcResponse,
    RpcResult, VERSION, read_json_line, validate_version, write_json_line,
};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use nix::unistd::{Gid, chown, geteuid};
use std::fs;
use std::io::{self, BufReader};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Per-socket role: the socket a client connected on bounds which ops it may call,
/// ahead of any uid-based privilege check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketRole {
    /// ctl.sock: full op set (root/gateway uid still required for write ops).
    Admin,
    /// dsh.sock: self-serve attach/heartbeat/tree only; the peer can only act on itself.
    Dsh,
    /// ui.sock: delegated control plane for the gRPC adapter; read + whitelisted group writes.
    Ui,
}

fn role_allows(role: SocketRole, method: &RpcMethod) -> bool {
    match role {
        SocketRole::Admin => !matches!(
            method,
            RpcMethod::AttachSelf
                | RpcMethod::StatusSelf
                | RpcMethod::TreeSelf
                | RpcMethod::RegisterSelf
        ),
        SocketRole::Dsh => matches!(
            method,
            RpcMethod::AttachSelf | RpcMethod::StatusSelf | RpcMethod::TreeSelf
        ),
        SocketRole::Ui => matches!(
            method,
            RpcMethod::Health
                | RpcMethod::GetCapabilities
                | RpcMethod::GetMetrics
                | RpcMethod::ListScopes
                | RpcMethod::ListTrees
                | RpcMethod::GetPolicy
                | RpcMethod::ValidatePolicy
                | RpcMethod::ApplyPolicy
                | RpcMethod::SetSwitches
        ),
    }
}

pub fn start_control(
    path: &Path,
    socket_group: Option<Gid>,
    runtime: Arc<Runtime>,
    hub: Arc<EventHub>,
    gateway_uid: Option<u32>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    // ctl.sock is root-only by default (0600); an explicit socket group widens it to 0660.
    let mode = if socket_group.is_some() { 0o660 } else { 0o600 };
    let listener = listen(path, socket_group, mode)?;
    thread::Builder::new()
        .name("censorguard-control".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let runtime = Arc::clone(&runtime);
                        let hub = Arc::clone(&hub);
                        let _ = thread::Builder::new()
                            .name("censorguard-control-client".into())
                            .spawn(move || {
                                handle_control(
                                    stream,
                                    &runtime,
                                    &hub,
                                    gateway_uid,
                                    SocketRole::Admin,
                                );
                            });
                    }
                    Err(error) => eprintln!("control accept failed: {error}"),
                }
            }
        })
}

pub fn start_dsh(
    path: &Path,
    runtime: Arc<Runtime>,
    hub: Arc<EventHub>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    // 0666: any local user may attach *itself*; SO_PEERCRED makes the pid unforgeable
    // and the role ACL restricts this socket to the self-serve ops.
    let listener = listen(path, None, 0o666)?;
    thread::Builder::new()
        .name("censorguard-dsh".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let runtime = Arc::clone(&runtime);
                        let hub = Arc::clone(&hub);
                        let _ = thread::Builder::new()
                            .name("censorguard-dsh-client".into())
                            .spawn(move || {
                                handle_control(stream, &runtime, &hub, None, SocketRole::Dsh);
                            });
                    }
                    Err(error) => eprintln!("dsh accept failed: {error}"),
                }
            }
        })
}

pub fn start_ui(
    path: &Path,
    runtime: Arc<Runtime>,
    hub: Arc<EventHub>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    // 0666: the gRPC adapter runs unprivileged; the role ACL limits this socket to
    // reads plus whitelisted DSH group writes. Production may tighten via group.
    let listener = listen(path, None, 0o666)?;
    thread::Builder::new()
        .name("censorguard-ui".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let runtime = Arc::clone(&runtime);
                        let hub = Arc::clone(&hub);
                        let _ = thread::Builder::new()
                            .name("censorguard-ui-client".into())
                            .spawn(move || {
                                handle_control(stream, &runtime, &hub, None, SocketRole::Ui);
                            });
                    }
                    Err(error) => eprintln!("ui accept failed: {error}"),
                }
            }
        })
}

pub fn start_events(
    path: &Path,
    socket_group: Option<Gid>,
    hub: Arc<EventHub>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    // 0666 read-only event stream; subscribers only ever receive data.
    let mode = if socket_group.is_some() { 0o660 } else { 0o666 };
    let listener = listen(path, socket_group, mode)?;
    thread::Builder::new()
        .name("censorguard-events".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let receiver = hub.subscribe();
                        let _ = thread::Builder::new()
                            .name("censorguard-event-client".into())
                            .spawn(move || {
                                let mut stream = stream;
                                while let Ok(event) = receiver.recv() {
                                    if write_json_line(&mut stream, &event).is_err() {
                                        return;
                                    }
                                }
                            });
                    }
                    Err(error) => eprintln!("events accept failed: {error}"),
                }
            }
        })
}

pub fn start_launch(
    path: &Path,
    socket_group: Option<Gid>,
    runtime: Arc<Runtime>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    let listener = listen(path, socket_group, 0o660)?;
    thread::Builder::new()
        .name("censorguard-launch".into())
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let runtime = Arc::clone(&runtime);
                        let _ = thread::Builder::new()
                            .name("censorguard-launch-client".into())
                            .spawn(move || handle_launch(stream, &runtime));
                    }
                    Err(error) => eprintln!("launch accept failed: {error}"),
                }
            }
        })
}

fn handle_control(
    mut stream: UnixStream,
    runtime: &Runtime,
    hub: &EventHub,
    gateway_uid: Option<u32>,
    role: SocketRole,
) {
    let timeout = Some(Duration::from_secs(30));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let value = match read_json_line::<serde_json::Value>(&mut BufReader::new(&stream)) {
        Ok(value) => value,
        Err(error) => {
            let _ = write_json_line(&mut stream, &failure(format!("invalid request: {error}")));
            return;
        }
    };
    let credentials = match getsockopt(&stream, PeerCredentials) {
        Ok(credentials) => credentials,
        Err(error) => {
            let _ = write_json_line(
                &mut stream,
                &failure(format!("cannot read peer credentials: {error}")),
            );
            return;
        }
    };
    let peer_uid = credentials.uid();
    let peer_pid = u32::try_from(credentials.pid()).ok();
    if value.get("method").is_some() {
        let request_id = value
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let response = match serde_json::from_value::<RpcRequest>(value) {
            Ok(request) => {
                dispatch_rpc(runtime, hub, request, peer_uid, peer_pid, gateway_uid, role)
            }
            Err(error) => rpc_failure(
                request_id,
                RpcErrorCode::InvalidRequest,
                format!("invalid v2 request: {error}"),
                None,
            ),
        };
        let _ = write_json_line(&mut stream, &response);
    } else if role == SocketRole::Admin {
        let response = match serde_json::from_value::<Request>(value) {
            Ok(request) => {
                if let Err(error) = validate_version(request.version) {
                    failure(error.to_string())
                } else {
                    dispatch(runtime, hub, request, peer_uid, gateway_uid)
                }
            }
            Err(error) => failure(format!("invalid legacy request: {error}")),
        };
        let _ = write_json_line(&mut stream, &response);
    } else {
        let _ = write_json_line(
            &mut stream,
            &failure("legacy v1 protocol is only accepted on the control socket"),
        );
    }
}

fn handle_launch(mut stream: UnixStream, runtime: &Runtime) {
    let timeout = Some(Duration::from_secs(10));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let request = match read_json_line::<RpcRequest>(&mut BufReader::new(&stream)) {
        Ok(request) => request,
        Err(error) => {
            let response = rpc_failure(
                String::new(),
                RpcErrorCode::InvalidRequest,
                format!("invalid launcher request: {error}"),
                None,
            );
            let _ = write_json_line(&mut stream, &response);
            return;
        }
    };
    let credentials = match getsockopt(&stream, PeerCredentials) {
        Ok(credentials) => credentials,
        Err(error) => {
            let response = rpc_failure(
                request.request_id,
                RpcErrorCode::Internal,
                format!("cannot read launcher credentials: {error}"),
                None,
            );
            let _ = write_json_line(&mut stream, &response);
            return;
        }
    };
    let response = if request.method != RpcMethod::RegisterSelf {
        rpc_failure(
            request.request_id,
            RpcErrorCode::PermissionDenied,
            "launch socket only accepts register_self",
            None,
        )
    } else if let Err(error) = validate_version(request.version) {
        rpc_failure(
            request.request_id,
            RpcErrorCode::UnsupportedVersion,
            error.to_string(),
            None,
        )
    } else {
        let pid = match u32::try_from(credentials.pid()) {
            Ok(pid) => pid,
            Err(_) => {
                let response = rpc_failure(
                    request.request_id,
                    RpcErrorCode::InvalidRequest,
                    "launcher peer PID is invalid",
                    None,
                );
                let _ = write_json_line(&mut stream, &response);
                return;
            }
        };
        match runtime.register_self(
            pid,
            request.params.scope.as_deref().unwrap_or("default"),
            request.params.group.as_deref().unwrap_or(""),
            credentials.uid(),
        ) {
            Ok(result) => rpc_success(
                request.request_id,
                RpcResult {
                    response: Some(Response {
                        ok: true,
                        seeded: Some(result.seeded),
                        domain: Some(result.domain),
                        ..Response::default()
                    }),
                    revision: runtime.current_revision().ok(),
                    ..RpcResult::default()
                },
            ),
            Err(error) => runtime_rpc_failure(request.request_id, error),
        }
    };
    let _ = write_json_line(&mut stream, &response);
}

fn dispatch_rpc(
    runtime: &Runtime,
    hub: &EventHub,
    request: RpcRequest,
    peer_uid: u32,
    peer_pid: Option<u32>,
    gateway_uid: Option<u32>,
    role: SocketRole,
) -> RpcResponse {
    if let Err(error) = validate_version(request.version) {
        return rpc_failure(
            request.request_id,
            RpcErrorCode::UnsupportedVersion,
            error.to_string(),
            None,
        );
    }
    let request_id = request.request_id;
    let params = request.params;
    // The socket role ACL runs ahead of any uid-based privilege check.
    if !role_allows(role, &request.method) {
        return rpc_failure(
            request_id,
            RpcErrorCode::PermissionDenied,
            format!(
                "method {:?} is not allowed on the {role:?} socket",
                request.method
            ),
            None,
        );
    }
    match request.method {
        RpcMethod::RegisterSelf => rpc_failure(
            request_id,
            RpcErrorCode::PermissionDenied,
            "register_self is only accepted on the launch socket",
            None,
        ),
        RpcMethod::AttachSelf => {
            let Some(pid) = peer_pid else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::Internal,
                    "cannot determine peer pid",
                    None,
                );
            };
            let Some(group) = params
                .policy_group
                .as_deref()
                .filter(|group| !group.is_empty())
            else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "attach_self requires params.policy_group",
                    None,
                );
            };
            // The pid always comes from SO_PEERCRED; any client-supplied value is ignored.
            match runtime.attach_self(
                pid,
                group,
                params.instance_hint.as_deref(),
                params.seed,
                peer_uid,
            ) {
                Ok(result) => rpc_success(
                    request_id,
                    RpcResult {
                        response: Some(Response {
                            ok: true,
                            seeded: Some(result.seeded),
                            domain: Some(result.domain),
                            daemon_boot_id: runtime.boot_id().to_owned(),
                            hooks_healthy: Some(runtime.hooks_healthy()),
                            ..Response::default()
                        }),
                        revision: runtime.current_revision().ok(),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::StatusSelf => {
            let Some(pid) = peer_pid else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::Internal,
                    "cannot determine peer pid",
                    None,
                );
            };
            match runtime.status_self(pid) {
                Ok(domain) => rpc_success(
                    request_id,
                    RpcResult {
                        response: Some(Response {
                            ok: true,
                            domains: domain.into_iter().collect(),
                            daemon_boot_id: runtime.boot_id().to_owned(),
                            hooks_healthy: Some(runtime.hooks_healthy()),
                            ..Response::default()
                        }),
                        revision: runtime.current_revision().ok(),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::TreeSelf => {
            let Some(pid) = peer_pid else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::Internal,
                    "cannot determine peer pid",
                    None,
                );
            };
            match runtime.tree_self(pid) {
                Ok(tree) => rpc_success(
                    request_id,
                    RpcResult {
                        response: Some(Response {
                            ok: true,
                            trees: tree.into_iter().collect(),
                            daemon_boot_id: runtime.boot_id().to_owned(),
                            hooks_healthy: Some(runtime.hooks_healthy()),
                            ..Response::default()
                        }),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::GetCapabilities => rpc_success(
            request_id,
            RpcResult {
                revision: runtime.current_revision().ok(),
                capabilities: vec![
                    "protocol.v1-legacy".into(),
                    "protocol.v2-jsonrpc".into(),
                    "scope.policy-binding".into(),
                    "launcher.peer-credentials".into(),
                    "dsh.attach-self".into(),
                    "dsh.socket-roles".into(),
                    "event.sequence".into(),
                    "intent.file".into(),
                    "intent.exec".into(),
                    "intent.network-ipv4".into(),
                    "intent.network-ipv6".into(),
                    "policy.atomic-bank".into(),
                    "policy.rollback".into(),
                    "policy.read-current".into(),
                    "policy.group-scoped".into(),
                ],
                ..RpcResult::default()
            },
        ),
        RpcMethod::GetPolicy => {
            if let Some(group) = params.group.as_deref().filter(|group| !group.is_empty()) {
                // Single-group read (any group incl. __base__ is readable; writes are restricted).
                match runtime.group_policy(group) {
                    Ok((yaml, version, domains)) => rpc_success(
                        request_id,
                        RpcResult {
                            revision: Some(u64::from(version)),
                            policy_yaml: Some(yaml),
                            response: Some(Response {
                                ok: true,
                                groups: vec![censorguard_common::protocol::GroupDump {
                                    name: group.to_owned(),
                                    version,
                                    domains,
                                    definition:
                                        censorguard_common::protocol::PolicyDefinition::default(),
                                }],
                                ..Response::default()
                            }),
                            ..RpcResult::default()
                        },
                    ),
                    Err(error) => runtime_rpc_failure(request_id, error),
                }
            } else {
                match runtime.current_policy_yaml() {
                    Ok(policy_yaml) => rpc_success(
                        request_id,
                        RpcResult {
                            revision: runtime.current_revision().ok(),
                            policy_yaml: Some(policy_yaml),
                            ..RpcResult::default()
                        },
                    ),
                    Err(error) => runtime_rpc_failure(request_id, error),
                }
            }
        }
        RpcMethod::Health | RpcMethod::GetMetrics | RpcMethod::ListScopes => {
            let response = dispatch(
                runtime,
                hub,
                legacy_request(Operation::Status),
                peer_uid,
                gateway_uid,
            );
            rpc_success(
                request_id,
                RpcResult {
                    response: Some(response),
                    revision: runtime.current_revision().ok(),
                    ..RpcResult::default()
                },
            )
        }
        RpcMethod::ListTrees => {
            let response = dispatch(
                runtime,
                hub,
                legacy_request(Operation::Tree),
                peer_uid,
                gateway_uid,
            );
            rpc_success(
                request_id,
                RpcResult {
                    response: Some(response),
                    revision: runtime.current_revision().ok(),
                    ..RpcResult::default()
                },
            )
        }
        RpcMethod::EvaluateIntent => {
            match runtime.evaluate_intents(params.group.as_deref().unwrap_or(""), &params.intents) {
                Ok(decisions) => rpc_success(
                    request_id,
                    RpcResult {
                        decisions,
                        revision: runtime.current_revision().ok(),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::ValidatePolicy => {
            let Some(yaml) = params.policy_yaml.as_deref() else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "validate_policy requires params.policy_yaml",
                    None,
                );
            };
            if let Some(group) = params.group.as_deref().filter(|group| !group.is_empty()) {
                // Group fragment validation: compile-only dry run, no data-plane change.
                if role == SocketRole::Ui && !runtime.is_dsh_group_allowed(group) {
                    return rpc_failure(
                        request_id,
                        RpcErrorCode::PermissionDenied,
                        format!("policy group {group:?} is not editable on this socket"),
                        None,
                    );
                }
                match runtime.reload(None, Some(yaml), Some(group), true) {
                    Ok(result) => reload_rpc_success(request_id, result),
                    Err(error) => runtime_rpc_failure(request_id, error),
                }
            } else {
                match runtime.validate_policy_yaml(yaml) {
                    Ok(result) => reload_rpc_success(request_id, result),
                    Err(error) => runtime_rpc_failure(request_id, error),
                }
            }
        }
        RpcMethod::ApplyPolicy => {
            if let Some(group) = params.group.as_deref().filter(|group| !group.is_empty()) {
                // Delegated group-scoped apply: whitelisted DSH groups only.
                // Path selection keys on the group param (same as validate_policy) so a
                // privileged peer bridging the delegated surface — the daemon-spawned
                // censorguard-grpc child runs as root and shows peer_uid 0 on ui.sock —
                // still speaks the group protocol instead of the full-policy one.
                if role != SocketRole::Ui && !is_privileged(peer_uid, gateway_uid) {
                    return rpc_failure(
                        request_id,
                        RpcErrorCode::PermissionDenied,
                        "group-scoped apply_policy requires the delegated ui socket or root",
                        None,
                    );
                }
                if role == SocketRole::Ui && !runtime.is_dsh_group_allowed(group) {
                    return rpc_failure(
                        request_id,
                        RpcErrorCode::PermissionDenied,
                        format!("policy group {group:?} is not in the DSH self-serve whitelist"),
                        None,
                    );
                }
                let Some(yaml) = params.policy_yaml.as_deref() else {
                    return rpc_failure(
                        request_id,
                        RpcErrorCode::InvalidRequest,
                        "apply_policy requires params.policy_yaml",
                        None,
                    );
                };
                if let Some(expected) = params.expected_revision
                    && let Err(error) = runtime.check_revision(expected)
                {
                    return runtime_rpc_failure(request_id, error);
                }
                return match runtime.reload(None, Some(yaml), Some(group), false) {
                    Ok(result) => reload_rpc_success(request_id, result),
                    Err(error) => runtime_rpc_failure(request_id, error),
                };
            }
            // Full-policy apply (baseline + all groups): privileged peers only.
            if !is_privileged(peer_uid, gateway_uid) {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::PermissionDenied,
                    "apply_policy requires root",
                    None,
                );
            }
            let (Some(yaml), Some(expected)) =
                (params.policy_yaml.as_deref(), params.expected_revision)
            else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "apply_policy requires policy_yaml and expected_revision",
                    None,
                );
            };
            match runtime.apply_policy_yaml(yaml, expected, params.idempotency_key.as_deref()) {
                Ok(result) => reload_rpc_success(request_id, result),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::RollbackPolicy => {
            if !is_privileged(peer_uid, gateway_uid) {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::PermissionDenied,
                    "rollback_policy requires root",
                    None,
                );
            }
            let (Some(revision), Some(expected)) = (params.revision, params.expected_revision)
            else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "rollback_policy requires revision and expected_revision",
                    None,
                );
            };
            match runtime.rollback_policy(revision, expected, params.idempotency_key.as_deref()) {
                Ok(result) => reload_rpc_success(request_id, result),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::RebindScope => {
            if !is_privileged(peer_uid, gateway_uid) {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::PermissionDenied,
                    "rebind_scope requires root",
                    None,
                );
            }
            let (Some(scope), Some(group)) = (params.scope.as_deref(), params.group.as_deref())
            else {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "rebind_scope requires scope and group",
                    None,
                );
            };
            match runtime.rebind_scope(scope, group) {
                Ok(domain) => rpc_success(
                    request_id,
                    RpcResult {
                        response: Some(Response {
                            ok: true,
                            domain: Some(domain),
                            ..Response::default()
                        }),
                        revision: runtime.current_revision().ok(),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::SetSwitches => {
            // Root/gateway on ctl.sock or any peer on the delegated ui.sock surface.
            if !is_privileged(peer_uid, gateway_uid) && role != SocketRole::Ui {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::PermissionDenied,
                    "set_switches requires root",
                    None,
                );
            }
            let updates = [
                params.enable_file,
                params.enable_exec,
                params.enable_net,
                params.audit_file,
                params.audit_exec,
                params.audit_net,
            ];
            if updates.iter().all(Option::is_none) {
                return rpc_failure(
                    request_id,
                    RpcErrorCode::InvalidRequest,
                    "set_switches requires at least one switch field",
                    None,
                );
            }
            let [
                enable_file,
                enable_exec,
                enable_net,
                audit_file,
                audit_exec,
                audit_net,
            ] = updates;
            match runtime.set_switches(
                enable_file,
                enable_exec,
                enable_net,
                audit_file,
                audit_exec,
                audit_net,
            ) {
                Ok(switches) => rpc_success(
                    request_id,
                    RpcResult {
                        response: Some(switch_response(switches)),
                        revision: runtime.current_revision().ok(),
                        ..RpcResult::default()
                    },
                ),
                Err(error) => runtime_rpc_failure(request_id, error),
            }
        }
        RpcMethod::CloseScope => rpc_failure(
            request_id,
            RpcErrorCode::InvalidRequest,
            "close_scope is not available until owner-token lifecycle is enabled",
            None,
        ),
    }
}

fn legacy_request(op: Operation) -> Request {
    Request {
        version: VERSION,
        op,
        pid: None,
        domain: None,
        seed: false,
        policy_file: None,
        policy_yaml: None,
        group: None,
        dry_run: false,
        enable_file: None,
        enable_exec: None,
        enable_net: None,
        audit_file: None,
        audit_exec: None,
        audit_net: None,
    }
}

fn switch_response(switches: crate::runtime::SwitchState) -> Response {
    Response {
        ok: true,
        enable_file: switches.enables[0],
        enable_exec: switches.enables[1],
        enable_net: switches.enables[2],
        audit_file: switches.audits[0],
        audit_exec: switches.audits[1],
        audit_net: switches.audits[2],
        ..Response::default()
    }
}

fn reload_rpc_success(request_id: String, result: crate::runtime::ReloadResult) -> RpcResponse {
    rpc_success(
        request_id,
        RpcResult {
            revision: Some(u64::from(result.version)),
            response: Some(Response {
                ok: true,
                reload_gen: result.generation,
                version: result.version,
                active_bank: result.active_bank,
                changed: result.changed,
                enable_file: result.enables[0],
                enable_exec: result.enables[1],
                enable_net: result.enables[2],
                audit_file: result.audits[0],
                audit_exec: result.audits[1],
                audit_net: result.audits[2],
                ..Response::default()
            }),
            ..RpcResult::default()
        },
    )
}

fn runtime_rpc_failure(request_id: String, error: RuntimeError) -> RpcResponse {
    let (code, current_revision) = match &error {
        RuntimeError::RevisionConflict { current, .. } => {
            (RpcErrorCode::RevisionConflict, Some(*current))
        }
        RuntimeError::Policy(_) => (RpcErrorCode::PolicyInvalid, None),
        RuntimeError::Kernel(censorguard_kernel::KernelError::TooManyPolicyGroups { .. }) => {
            (RpcErrorCode::PolicyCapacityExceeded, None)
        }
        RuntimeError::MissingGroup { .. } => (RpcErrorCode::UnknownGroup, None),
        RuntimeError::UnknownDomain(_) => (RpcErrorCode::UnknownScope, None),
        RuntimeError::DuplicateRoot(_) => (RpcErrorCode::ScopeAlreadyRegistered, None),
        RuntimeError::UnauthorizedPid { .. }
        | RuntimeError::UnauthorizedRoot { .. }
        | RuntimeError::GroupNotAllowed(_) => (RpcErrorCode::PermissionDenied, None),
        _ => (RpcErrorCode::Internal, None),
    };
    rpc_failure(request_id, code, error.to_string(), current_revision)
}

fn rpc_success(request_id: String, result: RpcResult) -> RpcResponse {
    RpcResponse {
        version: VERSION,
        request_id,
        ok: true,
        result: Some(result),
        error: None,
    }
}

fn rpc_failure(
    request_id: String,
    code: RpcErrorCode,
    message: impl Into<String>,
    current_revision: Option<u64>,
) -> RpcResponse {
    RpcResponse {
        version: VERSION,
        request_id,
        ok: false,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
            current_revision,
        }),
    }
}

fn dispatch(
    runtime: &Runtime,
    hub: &EventHub,
    request: Request,
    peer_uid: u32,
    gateway_uid: Option<u32>,
) -> Response {
    if request.op.requires_root() && !is_privileged(peer_uid, gateway_uid) {
        return failure(format!(
            "operation {:?} requires root; peer uid={peer_uid}",
            request.op
        ));
    }
    match request.op {
        Operation::Track => {
            let Some(pid) = request.pid else {
                return failure("track requires pid");
            };
            match runtime.track(
                pid,
                request.domain.as_deref().unwrap_or("default"),
                request.seed,
                peer_uid,
            ) {
                Ok(result) => Response {
                    ok: true,
                    seeded: Some(result.seeded),
                    domain: Some(result.domain),
                    ..Response::default()
                },
                Err(error) => failure(error.to_string()),
            }
        }
        Operation::Untrack => {
            let Some(pid) = request.pid else {
                return failure("untrack requires pid");
            };
            match runtime.untrack(pid, peer_uid) {
                Ok(()) => success(),
                Err(error) => failure(error.to_string()),
            }
        }
        Operation::Status => match runtime.status() {
            Ok(status) => {
                let events = hub.stats();
                Response {
                    ok: true,
                    roots: status.roots,
                    tracked: status.tracked,
                    pid_pending: status.pid_pending,
                    pid_generations: status.pid_generations,
                    pid_tracked_capacity: status.pid_tracked_capacity,
                    pid_pending_capacity: status.pid_pending_capacity,
                    pid_start_update_failures: status.pid_start_update_failures,
                    pid_tracked_update_failures: status.pid_tracked_update_failures,
                    pid_pending_update_failures: status.pid_pending_update_failures,
                    pid_pending_fallbacks: status.pid_pending_fallbacks,
                    scope_policy_lookup_failures: status.scope_policy_lookup_failures,
                    scopes: status.scopes,
                    scope_capacity: status.scope_capacity,
                    domains: status.domains,
                    reload_gen: status.reload_generation,
                    version: status.version,
                    active_bank: status.active_bank,
                    enable_file: status.enables[0],
                    enable_exec: status.enables[1],
                    enable_net: status.enables[2],
                    audit_file: status.audits[0],
                    audit_exec: status.audits[1],
                    audit_net: status.audits[2],
                    last_reload_time_ms: status.last_reload_time_ms,
                    last_reload_error: status.last_reload_error,
                    event_kernel_dropped: events.kernel_dropped,
                    event_reader_dropped: events.reader_dropped,
                    event_subscriber_dropped: events.subscriber_dropped,
                    event_subscribers: events.subscribers,
                    daemon_boot_id: runtime.boot_id().to_owned(),
                    hooks_healthy: Some(runtime.hooks_healthy()),
                    ..Response::default()
                }
            }
            Err(error) => failure(error.to_string()),
        },
        Operation::Reload => match runtime.reload(
            request.policy_file.as_deref(),
            request.policy_yaml.as_deref(),
            request.group.as_deref(),
            request.dry_run,
        ) {
            Ok(result) => Response {
                ok: true,
                reload_gen: result.generation,
                version: result.version,
                active_bank: result.active_bank,
                changed: result.changed,
                enable_file: result.enables[0],
                enable_exec: result.enables[1],
                enable_net: result.enables[2],
                audit_file: result.audits[0],
                audit_exec: result.audits[1],
                audit_net: result.audits[2],
                ..Response::default()
            },
            Err(error) => failure(error.to_string()),
        },
        Operation::Bind => {
            let Some(domain) = request.domain.as_deref() else {
                return failure("bind requires domain");
            };
            let Some(group) = request.group.as_deref() else {
                return failure("bind requires group");
            };
            match runtime.bind_domain(domain, group, request.dry_run) {
                Ok(result) => Response {
                    ok: true,
                    reload_gen: result.generation,
                    version: result.version,
                    active_bank: result.active_bank,
                    changed: result.changed,
                    enable_file: result.enables[0],
                    enable_exec: result.enables[1],
                    enable_net: result.enables[2],
                    audit_file: result.audits[0],
                    audit_exec: result.audits[1],
                    audit_net: result.audits[2],
                    ..Response::default()
                },
                Err(error) => failure(error.to_string()),
            }
        }
        Operation::PolicyDump => match runtime.policy_dump() {
            Ok((switches, groups, dns)) => Response {
                ok: true,
                groups,
                enable_file: switches.enables[0],
                enable_exec: switches.enables[1],
                enable_net: switches.enables[2],
                audit_file: switches.audits[0],
                audit_exec: switches.audits[1],
                audit_net: switches.audits[2],
                dns,
                ..Response::default()
            },
            Err(error) => failure(error.to_string()),
        },
        Operation::SetSwitches => {
            match runtime.set_switches(
                request.enable_file,
                request.enable_exec,
                request.enable_net,
                request.audit_file,
                request.audit_exec,
                request.audit_net,
            ) {
                Ok(switches) => switch_response(switches),
                Err(error) => failure(error.to_string()),
            }
        }
        Operation::Tree => match runtime.trees() {
            Ok(trees) => Response {
                ok: true,
                trees,
                ..Response::default()
            },
            Err(error) => failure(error.to_string()),
        },
    }
}

fn is_privileged(peer_uid: u32, gateway_uid: Option<u32>) -> bool {
    peer_uid == 0 || gateway_uid == Some(peer_uid)
}

fn success() -> Response {
    Response {
        ok: true,
        ..Response::default()
    }
}

fn failure(message: impl Into<String>) -> Response {
    Response {
        ok: false,
        error: Some(message.into()),
        ..Response::default()
    }
}

fn listen(path: &Path, socket_group: Option<Gid>, mode: u32) -> Result<UnixListener, io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    fs::create_dir_all(parent)?;
    validate_socket_parent(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to replace non-socket path {}", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    let configured = socket_group
        .map_or(Ok(()), |gid| {
            chown(path, None, Some(gid)).map_err(io::Error::from)
        })
        .and_then(|()| fs::set_permissions(path, fs::Permissions::from_mode(mode)));
    if let Err(error) = configured {
        drop(listener);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(listener)
}

fn validate_socket_parent(parent: &Path) -> Result<(), io::Error> {
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("socket parent is not a directory: {}", parent.display()),
        ));
    }
    let expected_uid = geteuid().as_raw();
    let writable_by_others = metadata.mode() & 0o022 != 0;
    if metadata.uid() != expected_uid || writable_by_others {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket parent must be owned by uid {expected_uid} and not writable by group/other: {}",
                parent.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SocketRole, is_privileged, role_allows};
    use censorguard_common::protocol::RpcMethod;

    #[test]
    fn only_root_or_the_explicit_gateway_uid_is_privileged() {
        assert!(is_privileged(0, None));
        assert!(is_privileged(1001, Some(1001)));
        assert!(!is_privileged(1002, Some(1001)));
        assert!(!is_privileged(1001, None));
    }

    #[test]
    fn dsh_socket_only_serves_self_ops() {
        for method in [
            RpcMethod::AttachSelf,
            RpcMethod::StatusSelf,
            RpcMethod::TreeSelf,
        ] {
            assert!(role_allows(SocketRole::Dsh, &method));
            assert!(!role_allows(SocketRole::Admin, &method));
            assert!(!role_allows(SocketRole::Ui, &method));
        }
        for method in [
            RpcMethod::ApplyPolicy,
            RpcMethod::Health,
            RpcMethod::RegisterSelf,
        ] {
            assert!(!role_allows(SocketRole::Dsh, &method));
        }
    }

    #[test]
    fn ui_socket_serves_reads_and_group_policy_writes() {
        for method in [
            RpcMethod::Health,
            RpcMethod::GetCapabilities,
            RpcMethod::GetMetrics,
            RpcMethod::ListScopes,
            RpcMethod::ListTrees,
            RpcMethod::GetPolicy,
            RpcMethod::ValidatePolicy,
            RpcMethod::ApplyPolicy,
        ] {
            assert!(role_allows(SocketRole::Ui, &method));
        }
        for method in [
            RpcMethod::RollbackPolicy,
            RpcMethod::RebindScope,
            RpcMethod::EvaluateIntent,
            RpcMethod::AttachSelf,
            RpcMethod::RegisterSelf,
        ] {
            assert!(!role_allows(SocketRole::Ui, &method));
        }
        // The admin socket keeps the full legacy op set except self-serve/launcher ops.
        assert!(role_allows(SocketRole::Admin, &RpcMethod::ApplyPolicy));
        assert!(role_allows(SocketRole::Admin, &RpcMethod::EvaluateIntent));
        assert!(!role_allows(SocketRole::Admin, &RpcMethod::RegisterSelf));
    }
}
