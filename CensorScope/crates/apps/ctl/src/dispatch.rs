//! Dispatch from validated input to control-plane contracts.

use control_contract::command::{
    CallEndCommand, CallStartCommand, ControlCommand, DoctorCommand, ListTracesCommand,
    OperationStatusCommand, TrackAddCommand, TrackRemoveCommand,
};
use control_contract::reply::{ControlError, ControlReply};
use model_core::ids::RequestId;

use crate::args::CtlCommand;
use crate::transport::ControlClientPort;

/// Translate a parsed ctl command into its control-plane request.
pub fn dispatch(
    client: &mut impl ControlClientPort,
    request_id: RequestId,
    command: CtlCommand,
) -> Result<ControlReply, ControlError> {
    let control_command = match command {
        CtlCommand::TrackAdd {
            root,
            display_name,
            profile_name,
            tags,
            trace_id,
        } => ControlCommand::TrackAdd(TrackAddCommand {
            request_id,
            root,
            display_name,
            profile_name,
            tags,
            trace_id,
        }),
        CtlCommand::TrackRemove { selector } => ControlCommand::TrackRemove(TrackRemoveCommand {
            request_id,
            selector,
        }),
        CtlCommand::OperationStatus { operation_id } => {
            ControlCommand::OperationStatus(OperationStatusCommand {
                request_id,
                operation_id,
            })
        }
        CtlCommand::ListTraces { selector } => ControlCommand::ListTraces(ListTracesCommand {
            request_id,
            selector,
        }),
        CtlCommand::Doctor => ControlCommand::Doctor(DoctorCommand { request_id }),
        CtlCommand::CallStart {
            trace_id,
            session_id,
            call_id,
            host_pid,
            started_at,
        } => ControlCommand::CallStart(CallStartCommand {
            request_id,
            trace_id,
            session_id,
            call_id,
            host_pid,
            started_at,
        }),
        CtlCommand::CallEnd {
            trace_id,
            session_id,
            call_id,
            host_pid,
            ended_at,
            status,
        } => ControlCommand::CallEnd(CallEndCommand {
            request_id,
            trace_id,
            session_id,
            call_id,
            host_pid,
            ended_at,
            status,
        }),
        CtlCommand::Init { .. } => {
            return Err(ControlError::new(
                "invalid_dispatch",
                "init is handled by the local censorscopectl process",
            ));
        }
        CtlCommand::Export { .. } => {
            return Err(ControlError::new(
                "invalid_dispatch",
                "export is handled by the local censorscopectl process",
            ));
        }
    };

    client.send(control_command)
}
