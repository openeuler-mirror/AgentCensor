//! censorguard-grpc: unprivileged gRPC adapter in front of the daemon.
//!
//! The daemon never listens on the network. This process serves gRPC on
//! 127.0.0.1:50051 (loopback only by default) and forwards each call to the
//! daemon's ui.sock (control plane) / events.sock (event plane) using the
//! versioned JSON-lines protocol. The daemon's ui-socket role ACL is the
//! ultimate authorization backstop; this adapter adds a first line of defense
//! (e.g. refusing `__base__` writes up front).

// tonic mandates `Result<_, Status>` handler signatures; Status is large by design.
#![allow(clippy::result_large_err)]

use censorguard_common::protocol::{
    DEFAULT_EVENT_SOCKET, DEFAULT_UI_SOCKET, Event, ProtocolError, RpcError, RpcErrorCode,
    RpcMethod, RpcParams, RpcRequest, RpcResponse, VERSION, read_json_line, write_json_line,
};
use std::collections::HashSet;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Response, Status, transport::Server};

#[allow(clippy::all)]
pub mod pb {
    tonic::include_proto!("censorguard.v1");
}

const BASE_GROUP: &str = "__base__";

/// One connection per call: the daemon answers a single JSON line per connection.
struct DaemonClient {
    sock: PathBuf,
    counter: AtomicU64,
}

impl DaemonClient {
    fn new(sock: PathBuf) -> Self {
        Self {
            sock,
            counter: AtomicU64::new(0),
        }
    }

    fn call(&self, method: RpcMethod, params: RpcParams) -> Result<RpcResponse, Status> {
        let request_id = format!(
            "grpc-{}-{}",
            std::process::id(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let request = RpcRequest {
            version: VERSION,
            request_id,
            method,
            params,
        };
        let exchange = (|| -> Result<RpcResponse, ProtocolError> {
            let stream = UnixStream::connect(&self.sock)?;
            stream.set_read_timeout(Some(Duration::from_secs(30)))?;
            stream.set_write_timeout(Some(Duration::from_secs(30)))?;
            write_json_line(&mut &stream, &request)?;
            read_json_line(&mut BufReader::new(&stream))
        })();
        exchange.map_err(|error| Status::unavailable(format!("daemon is unreachable: {error}")))
    }
}

/// Structured error-code mapping (daemon RpcErrorCode -> gRPC status code).
fn rpc_status(error: &RpcError) -> Status {
    let code = match error.code {
        RpcErrorCode::PermissionDenied => Code::PermissionDenied,
        RpcErrorCode::UnknownGroup | RpcErrorCode::UnknownScope => Code::NotFound,
        RpcErrorCode::PolicyInvalid | RpcErrorCode::InvalidRequest => Code::InvalidArgument,
        RpcErrorCode::RevisionConflict => Code::FailedPrecondition,
        RpcErrorCode::ScopeAlreadyRegistered => Code::AlreadyExists,
        RpcErrorCode::PolicyCapacityExceeded => Code::ResourceExhausted,
        RpcErrorCode::IntentUnsupported => Code::Unimplemented,
        RpcErrorCode::Internal => Code::Internal,
        _ => Code::FailedPrecondition,
    };
    Status::new(code, error.message.clone())
}

fn ok_result(response: RpcResponse) -> Result<censorguard_common::protocol::RpcResult, Status> {
    if response.ok {
        Ok(response.result.unwrap_or_default())
    } else {
        match response.error {
            Some(error) => Err(rpc_status(&error)),
            None => Err(Status::internal(
                "daemon returned a failure without an error",
            )),
        }
    }
}

struct Adapter {
    client: Arc<DaemonClient>,
    ev_sock: PathBuf,
}

impl Adapter {
    async fn call(
        &self,
        method: RpcMethod,
        params: RpcParams,
    ) -> Result<censorguard_common::protocol::RpcResult, Status> {
        let client = Arc::clone(&self.client);
        let response = tokio::task::spawn_blocking(move || client.call(method, params))
            .await
            .map_err(|error| Status::internal(format!("call task failed: {error}")))??;
        ok_result(response)
    }
}

fn pb_domain(info: &censorguard_common::protocol::DomainInfo) -> pb::DomainInfo {
    pb::DomainInfo {
        name: info.name.clone(),
        id: info.id,
        slot: info.slot,
        group: info.group.clone(),
        roots: i32::try_from(info.roots).unwrap_or(i32::MAX),
        version: info.version,
        draining: info.draining,
    }
}

fn pb_event(event: &Event) -> pb::Event {
    pb::Event {
        ts: event.ts.clone(),
        kind: event.kind,
        op: event.op,
        pid: event.pid,
        tgid: event.tgid,
        allowed: event.allowed,
        policy_version: event.policy_version,
        rule_version: event.rule_version,
        comm: event.comm.clone(),
        detail: event.detail.clone(),
        args: event.args.clone(),
        domain_id: event.domain_id,
        domain: event.domain.clone(),
        sequence: event.sequence,
        daemon_boot_id: event.daemon_boot_id.clone(),
        dropped_before: event.dropped_before,
    }
}

fn pb_status(result: &censorguard_common::protocol::RpcResult) -> Result<pb::StatusReply, Status> {
    let response = result.response.clone().unwrap_or_default();
    Ok(pb::StatusReply {
        roots: response.roots,
        tracked: i32::try_from(response.tracked).unwrap_or(i32::MAX),
        domains: response.domains.iter().map(pb_domain).collect(),
        reload_gen: response.reload_gen,
        last_reload_time: response
            .last_reload_time_ms
            .map(rfc3339_from_epoch_ms)
            .unwrap_or_default(),
        last_reload_error: response.last_reload_error.unwrap_or_default(),
        config: std::collections::HashMap::from([
            ("enable_file".to_owned(), response.enable_file),
            ("enable_exec".to_owned(), response.enable_exec),
            ("enable_net".to_owned(), response.enable_net),
        ]),
        daemon_boot_id: response.daemon_boot_id,
        hooks_healthy: response.hooks_healthy.unwrap_or(false),
    })
}

fn rfc3339_from_epoch_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let second_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u64, u64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u64, day as u64)
}

fn admin_only(operation: &str) -> Status {
    Status::permission_denied(format!(
        "{operation} is an admin operation; the gRPC role cannot perform it"
    ))
}

#[tonic::async_trait]
impl pb::censorguard_server::Censorguard for Adapter {
    async fn status(
        &self,
        _request: Request<pb::StatusReq>,
    ) -> Result<Response<pb::StatusReply>, Status> {
        let result = self.call(RpcMethod::Health, RpcParams::default()).await?;
        Ok(Response::new(pb_status(&result)?))
    }

    async fn track(
        &self,
        _request: Request<pb::TrackReq>,
    ) -> Result<Response<pb::TrackReply>, Status> {
        Err(admin_only("track"))
    }

    async fn untrack(
        &self,
        _request: Request<pb::UntrackReq>,
    ) -> Result<Response<pb::UntrackReply>, Status> {
        Err(admin_only("untrack"))
    }

    async fn set_config(
        &self,
        _request: Request<pb::SetConfigReq>,
    ) -> Result<Response<pb::SetConfigReply>, Status> {
        Err(admin_only("set_config"))
    }

    async fn set_domain_binding(
        &self,
        _request: Request<pb::BindReq>,
    ) -> Result<Response<pb::BindReply>, Status> {
        Err(admin_only("set_domain_binding"))
    }

    async fn remove_policy(
        &self,
        _request: Request<pb::RemovePolicyReq>,
    ) -> Result<Response<pb::RemovePolicyReply>, Status> {
        Err(admin_only("remove_policy"))
    }

    async fn get_config(
        &self,
        request: Request<pb::GetConfigReq>,
    ) -> Result<Response<pb::GetConfigReply>, Status> {
        let reply = self
            .status(Request::new(pb::StatusReq { context: None }))
            .await?;
        let _ = request;
        Ok(Response::new(pb::GetConfigReply {
            config: reply.into_inner().config,
        }))
    }

    async fn set_switches(
        &self,
        request: Request<pb::SetSwitchesReq>,
    ) -> Result<Response<pb::SetSwitchesReply>, Status> {
        let req = request.into_inner();
        let result = self
            .call(
                RpcMethod::SetSwitches,
                RpcParams {
                    enable_file: req.enable_file,
                    enable_exec: req.enable_exec,
                    enable_net: req.enable_net,
                    audit_file: req.audit_file,
                    audit_exec: req.audit_exec,
                    audit_net: req.audit_net,
                    ..RpcParams::default()
                },
            )
            .await?;
        let response = result.response.unwrap_or_default();
        Ok(Response::new(pb::SetSwitchesReply {
            enable_file: response.enable_file,
            enable_exec: response.enable_exec,
            enable_net: response.enable_net,
            audit_file: response.audit_file,
            audit_exec: response.audit_exec,
            audit_net: response.audit_net,
        }))
    }

    async fn get_policy(
        &self,
        request: Request<pb::GetPolicyReq>,
    ) -> Result<Response<pb::GetPolicyReply>, Status> {
        let name = request.into_inner().name;
        let name = if name.is_empty() {
            BASE_GROUP.to_owned()
        } else {
            name
        };
        let result = self
            .call(
                RpcMethod::GetPolicy,
                RpcParams {
                    group: Some(name.clone()),
                    ..RpcParams::default()
                },
            )
            .await?;
        let policy_yaml = result.policy_yaml.unwrap_or_default();
        // Extract the one-line rules for easy diffing; the YAML fragment stays
        // authoritative for the editor.
        let rules = serde_yaml::from_str::<censorguard_policy::GroupPolicy>(&policy_yaml)
            .map(|group| group.rules)
            .unwrap_or_default();
        let domains = result
            .response
            .and_then(|response| response.groups.into_iter().next())
            .map(|group| group.domains)
            .unwrap_or_default();
        Ok(Response::new(pb::GetPolicyReply {
            name,
            version: u32::try_from(result.revision.unwrap_or(0)).unwrap_or(u32::MAX),
            policy_yaml,
            rules,
            domains,
        }))
    }

    async fn validate_policy(
        &self,
        request: Request<pb::ValidatePolicyReq>,
    ) -> Result<Response<pb::ValidatePolicyReply>, Status> {
        let req = request.into_inner();
        if req.name.is_empty() || req.name == BASE_GROUP {
            return Err(Status::permission_denied(
                "validate_policy requires a non-__base__ group name",
            ));
        }
        let client = Arc::clone(&self.client);
        let name = req.name.clone();
        let response = tokio::task::spawn_blocking(move || {
            client.call(
                RpcMethod::ValidatePolicy,
                RpcParams {
                    group: Some(name),
                    policy_yaml: Some(req.policy_yaml),
                    ..RpcParams::default()
                },
            )
        })
        .await
        .map_err(|error| Status::internal(format!("call task failed: {error}")))??;
        match ok_result(response) {
            Ok(_) => Ok(Response::new(pb::ValidatePolicyReply {
                ok: true,
                errors: Vec::new(),
                affected: Vec::new(),
            })),
            Err(status) if status.code() == Code::InvalidArgument => {
                Ok(Response::new(pb::ValidatePolicyReply {
                    ok: false,
                    errors: vec![status.message().to_owned()],
                    affected: Vec::new(),
                }))
            }
            Err(status) => Err(status),
        }
    }

    async fn apply_policy(
        &self,
        request: Request<pb::ApplyPolicyReq>,
    ) -> Result<Response<pb::ApplyPolicyReply>, Status> {
        let req = request.into_inner();
        // First line of defense: the global baseline can only be changed by root on ctl.sock.
        if req.name.is_empty() || req.name == BASE_GROUP {
            return Err(Status::permission_denied(
                "__base__ is the global baseline; the gRPC role cannot modify it",
            ));
        }
        if req.dry_run {
            let reply = self
                .validate_policy(Request::new(pb::ValidatePolicyReq {
                    name: req.name.clone(),
                    policy_yaml: req.policy_yaml,
                    context: None,
                }))
                .await?
                .into_inner();
            if !reply.ok {
                return Err(Status::invalid_argument(reply.errors.join("\n")));
            }
            return Ok(Response::new(pb::ApplyPolicyReply {
                reload_gen: 0,
                name: req.name,
                version: 0,
                affected: Vec::new(),
            }));
        }
        let result = self
            .call(
                RpcMethod::ApplyPolicy,
                RpcParams {
                    group: Some(req.name.clone()),
                    policy_yaml: Some(req.policy_yaml),
                    ..RpcParams::default()
                },
            )
            .await?;
        let response = result.response.unwrap_or_default();
        Ok(Response::new(pb::ApplyPolicyReply {
            reload_gen: response.reload_gen,
            name: req.name,
            version: response.version,
            affected: Vec::new(),
        }))
    }

    type SubscribeEventsStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<pb::Event, Status>> + Send>>;

    async fn subscribe_events(
        &self,
        request: Request<pb::EventsReq>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        let req = request.into_inner();
        let domain_ids: HashSet<u64> = req.domain_ids.into_iter().collect();
        let kinds: HashSet<u32> = req.kinds.into_iter().collect();
        let allowed = req.allowed;
        let ev_sock = self.ev_sock.clone();
        // Each stream owns an independent events.sock subscription; the daemon-side
        // per-subscriber dropped_before counter therefore reflects this consumer.
        let (sender, receiver) = mpsc::channel::<Result<pb::Event, Status>>(256);
        std::thread::Builder::new()
            .name("censorguard-grpc-events".into())
            .spawn(move || {
                let stream = match UnixStream::connect(&ev_sock) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = sender.blocking_send(Err(Status::unavailable(format!(
                            "event socket is unreachable: {error}"
                        ))));
                        return;
                    }
                };
                let mut reader = BufReader::new(&stream);
                loop {
                    match read_json_line::<Event>(&mut reader) {
                        Ok(event) => {
                            if !domain_ids.is_empty() && !domain_ids.contains(&event.domain_id) {
                                continue;
                            }
                            if !kinds.is_empty() && !kinds.contains(&event.kind) {
                                continue;
                            }
                            if allowed.is_some_and(|allowed| allowed != event.allowed) {
                                continue;
                            }
                            if sender.blocking_send(Ok(pb_event(&event))).is_err() {
                                return; // client disconnected
                            }
                        }
                        // A single bad frame is skipped without killing the stream.
                        Err(ProtocolError::Json(_)) | Err(ProtocolError::FrameTooLarge { .. }) => {}
                        Err(error) => {
                            let _ = sender.blocking_send(Err(Status::unavailable(format!(
                                "event stream ended: {error}"
                            ))));
                            return;
                        }
                    }
                }
            })
            .map_err(|error| Status::internal(format!("cannot spawn event pump: {error}")))?;
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, Status> {
    args.next()
        .ok_or_else(|| Status::invalid_argument(format!("{name} requires a value")))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sock = PathBuf::from(DEFAULT_UI_SOCKET);
    let mut ev_sock = PathBuf::from(DEFAULT_EVENT_SOCKET);
    let mut listen = String::from("127.0.0.1:50051");
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--sock" => sock = PathBuf::from(next_value(&mut args, "--sock")?),
            "--ev-sock" => ev_sock = PathBuf::from(next_value(&mut args, "--ev-sock")?),
            "--listen" => listen = next_value(&mut args, "--listen")?,
            "-h" | "--help" => {
                println!(
                    "usage: censorguard-grpc [--sock PATH] [--ev-sock PATH] [--listen ADDR]\n\
                     defaults: --sock {DEFAULT_UI_SOCKET} --ev-sock {DEFAULT_EVENT_SOCKET} \\\n\
                     --listen 127.0.0.1:50051 (loopback only; add mTLS at the deployment layer \
                     for cross-machine access)"
                );
                return Ok(());
            }
            _ => {
                return Err(
                    Status::invalid_argument(format!("unknown argument {argument:?}")).into(),
                );
            }
        }
    }
    let addr: std::net::SocketAddr = listen.parse()?;
    let adapter = Adapter {
        client: Arc::new(DaemonClient::new(sock.clone())),
        ev_sock: ev_sock.clone(),
    };
    println!(
        "[READY] censorguard-grpc listening on {addr} (ui={}, events={})",
        sock.display(),
        ev_sock.display()
    );
    Server::builder()
        .add_service(pb::censorguard_server::CensorguardServer::new(adapter))
        .serve(addr)
        .await?;
    Ok(())
}

// Silence unused-import lint when the path is only used in doc comments.
#[allow(dead_code)]
fn doc_path_anchor(_: &Path) {}
