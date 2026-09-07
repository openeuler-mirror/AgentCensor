use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Duration;
use tempfile::TempDir;
use censorfs_core::codec::RecordKind;
use censorfs_core::control::{
    decode, request, ControlDispatcher, ExploreRequest, Operation, PeerCredentials,
    TicketWriteRequest, TxRequest, VariantAbortRequest, VariantAbortResult, VariantOpenRequest,
    VariantOpenResult, VariantPrepareRequest, VariantPrepareResult, VariantPublishRequest,
    VariantPublishResult, ViewPathRequest, ViewSelector, MAX_FILE_CONTENT,
};
use censorfs_core::fault::FaultInjectBackend;
use censorfs_core::*;

fn branch(value: &str) -> BranchId {
    BranchId::new(value).unwrap()
}
fn path(value: &str) -> LogicalPath {
    LogicalPath::from_utf8(value).unwrap()
}

fn initialize() -> (TempDir, CensorFs, BranchHead) {
    let root = TempDir::new().unwrap();
    let import = root.path().join("import");
    fs::create_dir(&import).unwrap();
    fs::write(import.join("shared.txt"), b"initial").unwrap();
    fs::create_dir(import.join("dir")).unwrap();
    fs::write(import.join("dir/keep.txt"), b"keep").unwrap();
    let fs = CensorFs::initialize(InitOptions {
        storage_root: root.path().join(".censorfs"),
        import_root: import,
        main_branch: branch("main"),
    })
    .unwrap();
    let head = fs.branch_head(&branch("main")).unwrap();
    (root, fs, head)
}

#[cfg(unix)]
#[test]
fn import_preflight_rejects_symlinks_and_special_files() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let symlink_root = TempDir::new().unwrap();
    let symlink_import = symlink_root.path().join("import");
    fs::create_dir(&symlink_import).unwrap();
    fs::write(symlink_import.join("target"), b"target").unwrap();
    symlink("target", symlink_import.join("link")).unwrap();
    let error = CensorFs::initialize(InitOptions {
        storage_root: symlink_root.path().join(".censorfs"),
        import_root: symlink_import,
        main_branch: branch("main"),
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);

    let special_root = TempDir::new().unwrap();
    let special_import = special_root.path().join("import");
    fs::create_dir(&special_import).unwrap();
    let _listener = UnixListener::bind(special_import.join("socket")).unwrap();
    let error = CensorFs::initialize(InitOptions {
        storage_root: special_root.path().join(".censorfs"),
        import_root: special_import,
        main_branch: branch("main"),
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
}

fn begin_ticket(fs: &CensorFs, branch_id: &str) -> (TxMeta, BeginTicketResult) {
    let tx = fs.begin_tx(RequestId::new(), "test").unwrap();
    let head = fs.branch_head(&branch(branch_id)).unwrap();
    let ticket = fs
        .begin_ticket(RequestId::new(), tx.tx_id, branch(branch_id), Some(head))
        .unwrap();
    let view = fs.open_ticket_view(ticket.ticket_id, 1000, 1000).unwrap();
    (tx, BeginTicketResult { ticket, view })
}

#[test]
fn control_plane_uses_peer_uid_for_transaction_authorization() {
    let (_root, fs, _head) = initialize();
    let dispatcher = ControlDispatcher::new(fs);
    let owner = PeerCredentials {
        pid: 1,
        uid: 1000,
        gid: 1000,
    };
    let other = PeerCredentials {
        pid: 2,
        uid: 1001,
        gid: 1001,
    };
    let begin_request_id = RequestId::new();
    let begin = request(Operation::BeginTx, begin_request_id, 0, &()).unwrap();
    let response = dispatcher.dispatch(owner, begin.clone());
    assert_eq!(response.status, 0);
    let tx: TxMeta = decode(&response.payload).unwrap();
    assert_eq!(tx.actor_id, "uid:1000");

    let stolen_retry = dispatcher.dispatch(other, begin);
    assert_eq!(stolen_retry.status, ErrorCode::AccessDenied as u32);

    let close = request(
        Operation::CloseTx,
        RequestId::new(),
        0,
        &TxRequest { tx_id: tx.tx_id },
    )
    .unwrap();
    let denied = dispatcher.dispatch(other, close);
    assert_eq!(denied.status, ErrorCode::AccessDenied as u32);
}

#[test]
fn test_file_rpc_enforces_owner_limit_and_read_only_selectors() {
    let (_root, fs, _head) = initialize();
    let dispatcher = ControlDispatcher::new(fs);
    let owner = PeerCredentials {
        pid: 1,
        uid: 1000,
        gid: 1000,
    };
    let other = PeerCredentials {
        pid: 2,
        uid: 1001,
        gid: 1001,
    };
    let explore = dispatcher.dispatch(
        owner,
        request(
            Operation::Explore,
            RequestId::new(),
            0,
            &ExploreRequest {
                branch_id: branch("main"),
            },
        )
        .unwrap(),
    );
    assert_eq!(explore.status, 0);
    let exploration: ExplorationResult = decode(&explore.payload).unwrap();
    let write_request = TicketWriteRequest {
        ticket_id: exploration.ticket.ticket_id,
        path: path("rpc.txt"),
        data: b"private".to_vec(),
    };
    let denied = dispatcher.dispatch(
        other,
        request(Operation::TestWrite, RequestId::new(), 0, &write_request).unwrap(),
    );
    assert_eq!(denied.status, ErrorCode::AccessDenied as u32);
    let written = dispatcher.dispatch(
        owner,
        request(Operation::TestWrite, RequestId::new(), 0, &write_request).unwrap(),
    );
    assert_eq!(written.status, 0);

    let private = dispatcher.dispatch(
        owner,
        request(
            Operation::TestCat,
            RequestId::new(),
            0,
            &ViewPathRequest {
                selector: ViewSelector::Ticket(exploration.ticket.ticket_id),
                path: path("rpc.txt"),
            },
        )
        .unwrap(),
    );
    assert_eq!(private.status, 0);
    assert_eq!(decode::<Vec<u8>>(&private.payload).unwrap(), b"private");
    let stable = dispatcher.dispatch(
        owner,
        request(
            Operation::TestCat,
            RequestId::new(),
            0,
            &ViewPathRequest {
                selector: ViewSelector::Branch(branch("main")),
                path: path("rpc.txt"),
            },
        )
        .unwrap(),
    );
    assert_eq!(stable.status, ErrorCode::NotFound as u32);

    let oversized = dispatcher.dispatch(
        owner,
        request(
            Operation::TestWrite,
            RequestId::new(),
            0,
            &TicketWriteRequest {
                ticket_id: exploration.ticket.ticket_id,
                path: path("too-large"),
                data: vec![0; MAX_FILE_CONTENT + 1],
            },
        )
        .unwrap(),
    );
    assert_eq!(oversized.status, ErrorCode::BadRequest as u32);
}

#[test]
fn parallel_worlds_open_prepare_publish_abort_and_stale_are_atomic() {
    let (_root, fs, initial_head) = initialize();
    let dispatcher = ControlDispatcher::new(fs.clone());
    let owner = PeerCredentials {
        pid: 7,
        uid: 1000,
        gid: 1000,
    };
    let mut worlds = Vec::new();
    for index in 1..=3 {
        let response = dispatcher.dispatch(
            owner,
            request(
                Operation::VariantOpen,
                RequestId::new(),
                0,
                &VariantOpenRequest {
                    branch_id: branch("main"),
                    expected_head: initial_head.clone(),
                    run_id: "payment-fix".into(),
                    variant_id: format!("world-{index}"),
                },
            )
            .unwrap(),
        );
        assert_eq!(response.status, 0, "{}", response.message);
        worlds.push(decode::<VariantOpenResult>(&response.payload).unwrap());
    }

    assert!(worlds
        .iter()
        .all(|world| world.ticket.base_generation == initial_head.generation_id));
    for (index, world) in worlds.iter().enumerate() {
        fs.view_engine()
            .write_file(
                world.view.view_id,
                path("shared.txt"),
                format!("candidate-{}", index + 1).as_bytes(),
            )
            .unwrap();
        assert_eq!(
            fs.view_engine()
                .read_file(world.view.view_id, &path("shared.txt"))
                .unwrap(),
            format!("candidate-{}", index + 1).as_bytes()
        );
    }
    let stable = fs.open_branch_view(&branch("main"), 1000, 1000).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(stable.view_id, &path("shared.txt"))
            .unwrap(),
        b"initial"
    );
    fs.close_view(stable.view_id, 1000).unwrap();

    let mut prepared = Vec::new();
    for world in &worlds {
        let response = dispatcher.dispatch(
            owner,
            request(
                Operation::VariantPrepare,
                RequestId::new(),
                0,
                &VariantPrepareRequest {
                    ticket_id: world.ticket.ticket_id,
                    view_id: Some(world.view.view_id),
                    run_id: world.run_id.clone(),
                    variant_id: world.variant_id.clone(),
                    timeout_ms: 1_000,
                    max_diff_file_bytes: 64 * 1024,
                },
            )
            .unwrap(),
        );
        assert_eq!(response.status, 0, "{}", response.message);
        let result = decode::<VariantPrepareResult>(&response.payload).unwrap();
        assert_eq!(result.path_diff.len(), 1);
        let file = &result.text_diff.files[0];
        assert_eq!(file.disposition, TextDiffDisposition::Unified);
        assert!(file.patch.as_ref().unwrap().contains("candidate-"));
        prepared.push(result);
    }

    let winner = dispatcher.dispatch(
        owner,
        request(
            Operation::VariantPublish,
            RequestId::new(),
            0,
            &VariantPublishRequest {
                candidate_id: prepared[1].candidate.candidate_id,
                expected_head: initial_head.clone(),
                decision_id: "user-selected-world-2".into(),
                run_id: worlds[1].run_id.clone(),
                variant_id: worlds[1].variant_id.clone(),
            },
        )
        .unwrap(),
    );
    assert_eq!(winner.status, 0, "{}", winner.message);
    let winner = decode::<VariantPublishResult>(&winner.payload).unwrap();
    assert_eq!(winner.receipt.head_seq, initial_head.head_seq + 1);

    let stale = dispatcher.dispatch(
        owner,
        request(
            Operation::VariantPublish,
            RequestId::new(),
            0,
            &VariantPublishRequest {
                candidate_id: prepared[0].candidate.candidate_id,
                expected_head: initial_head.clone(),
                decision_id: "must-not-overwrite".into(),
                run_id: worlds[0].run_id.clone(),
                variant_id: worlds[0].variant_id.clone(),
            },
        )
        .unwrap(),
    );
    assert_eq!(stale.status, ErrorCode::HeadChanged as u32);

    for index in [0, 2] {
        let response = dispatcher.dispatch(
            owner,
            request(
                Operation::VariantAbort,
                RequestId::new(),
                0,
                &VariantAbortRequest {
                    ticket_id: worlds[index].ticket.ticket_id,
                    view_id: Some(worlds[index].view.view_id),
                    run_id: worlds[index].run_id.clone(),
                    variant_id: worlds[index].variant_id.clone(),
                },
            )
            .unwrap(),
        );
        assert_eq!(response.status, 0, "{}", response.message);
        let aborted = decode::<VariantAbortResult>(&response.payload).unwrap();
        assert_eq!(aborted.ticket.state, TicketState::Aborted);
        assert_eq!(aborted.tx.state, TxState::Aborted);
    }

    let head = fs.branch_head(&branch("main")).unwrap();
    assert_eq!(head.head_seq, initial_head.head_seq + 1);
    let stable = fs.open_branch_view(&branch("main"), 1000, 1000).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(stable.view_id, &path("shared.txt"))
            .unwrap(),
        b"candidate-2"
    );
}

#[test]
fn variant_request_id_replays_exactly_and_rejects_changed_input() {
    let (_root, fs, initial_head) = initialize();
    let dispatcher = ControlDispatcher::new(fs);
    let owner = PeerCredentials {
        pid: 9,
        uid: 1000,
        gid: 1000,
    };
    let request_id = RequestId::new();
    let input = VariantOpenRequest {
        branch_id: branch("main"),
        expected_head: initial_head,
        run_id: "retry-run".into(),
        variant_id: "minimal".into(),
    };
    let first = dispatcher.dispatch(
        owner,
        request(Operation::VariantOpen, request_id, 0, &input).unwrap(),
    );
    assert_eq!(first.status, 0, "{}", first.message);
    let first = decode::<VariantOpenResult>(&first.payload).unwrap();
    let replay = dispatcher.dispatch(
        owner,
        request(Operation::VariantOpen, request_id, 0, &input).unwrap(),
    );
    assert_eq!(replay.status, 0, "{}", replay.message);
    let replay = decode::<VariantOpenResult>(&replay.payload).unwrap();
    assert_eq!(replay.tx.tx_id, first.tx.tx_id);
    assert_eq!(replay.ticket.ticket_id, first.ticket.ticket_id);
    assert_eq!(replay.view.view_id, first.view.view_id);

    let changed = dispatcher.dispatch(
        owner,
        request(
            Operation::VariantOpen,
            request_id,
            0,
            &VariantOpenRequest {
                variant_id: "different".into(),
                ..input
            },
        )
        .unwrap(),
    );
    assert_eq!(changed.status, ErrorCode::AlreadyExistsDifferent as u32);
}

#[test]
fn text_diff_summarizes_binary_and_oversized_files_without_embedding_them() {
    let (_root, fs, initial_head) = initialize();
    let (_tx, ticket) = begin_ticket(&fs, "main");
    fs.view_engine()
        .create_file(
            ticket.view.view_id,
            path("binary.dat"),
            b"binary\0payload",
            0o644,
            true,
        )
        .unwrap();
    fs.view_engine()
        .create_file(
            ticket.view.view_id,
            path("large.txt"),
            b"larger than the configured text diff limit",
            0o644,
            true,
        )
        .unwrap();
    fs.close_view(ticket.view.view_id, 1000).unwrap();
    let prepared = fs
        .prepare_ticket(
            RequestId::new(),
            ticket.ticket.ticket_id,
            Duration::from_secs(1),
        )
        .unwrap();
    let report = fs
        .text_diff(
            initial_head.generation_id,
            prepared.generation.generation_id,
            8,
        )
        .unwrap();
    let binary = report
        .files
        .iter()
        .find(|file| file.path == path("binary.dat"))
        .unwrap();
    assert_eq!(binary.disposition, TextDiffDisposition::TooLarge);
    assert!(binary.patch.is_none());
    let large = report
        .files
        .iter()
        .find(|file| file.path == path("large.txt"))
        .unwrap();
    assert_eq!(large.disposition, TextDiffDisposition::TooLarge);
    assert!(large.patch.is_none());

    let binary_report = fs
        .text_diff(
            initial_head.generation_id,
            prepared.generation.generation_id,
            64,
        )
        .unwrap();
    let binary = binary_report
        .files
        .iter()
        .find(|file| file.path == path("binary.dat"))
        .unwrap();
    assert_eq!(binary.disposition, TextDiffDisposition::Binary);
    assert!(binary.old_digest.is_none());
    assert!(binary.new_digest.is_some());
}

#[cfg(unix)]
#[test]
fn persistence_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let (root, _fs, _head) = initialize();
    let storage = root.path().join(".censorfs");
    assert_eq!(
        fs::metadata(&storage).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for directory in censorfs_core::persist::SUBDIRS {
        assert_eq!(
            fs::metadata(storage.join(directory))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "{directory} was not private"
        );
    }
}

#[test]
fn publish_and_rollback_create_immutable_generations() {
    let (_root, fs, initial_head) = initialize();
    let historical = fs.open_branch_view(&branch("main"), 1000, 1000).unwrap();
    let (tx, ticket) = begin_ticket(&fs, "main");
    fs.view_engine()
        .write_file(ticket.view.view_id, path("shared.txt"), b"updated")
        .unwrap();
    fs.view_engine()
        .create_file(ticket.view.view_id, path("new.txt"), b"new", 0o640, true)
        .unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(historical.view_id, &path("shared.txt"))
            .unwrap(),
        b"initial"
    );

    let prepared = fs
        .prepare_ticket(
            RequestId::new(),
            ticket.ticket.ticket_id,
            Duration::from_secs(1),
        )
        .unwrap();
    let candidate_view = fs
        .open_candidate_view(prepared.candidate.candidate_id, 1000, 1000)
        .unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(candidate_view.view_id, &path("shared.txt"))
            .unwrap(),
        b"updated"
    );
    let receipt = fs
        .publish(
            RequestId::new(),
            prepared.candidate.candidate_id,
            "approve",
            initial_head.clone(),
        )
        .unwrap();
    assert_ne!(receipt.new_generation, initial_head.generation_id);
    assert_eq!(
        fs.resolve_fork_point(tx.tx_id, ticket.ticket.ticket_id)
            .unwrap(),
        receipt.new_generation
    );

    let rollback_tx = fs.begin_tx(RequestId::new(), "rollback").unwrap();
    let current = fs.branch_head(&branch("main")).unwrap();
    let rollback = fs
        .build_rollback_candidate(
            RequestId::new(),
            rollback_tx.tx_id,
            branch("main"),
            initial_head.generation_id,
            current.clone(),
        )
        .unwrap();
    let rollback_receipt = fs
        .publish(
            RequestId::new(),
            rollback.candidate.candidate_id,
            "rollback",
            current,
        )
        .unwrap();
    assert_ne!(rollback_receipt.new_generation, initial_head.generation_id);
    let rollback_meta = fs.generation(rollback_receipt.new_generation).unwrap();
    assert_eq!(rollback_meta.kind, GenerationKind::Rollback);
    assert_eq!(
        rollback_meta.rollback_target,
        Some(initial_head.generation_id)
    );
    assert_eq!(rollback_meta.parents, vec![receipt.new_generation]);

    let original = fs.manifest(initial_head.generation_id).unwrap();
    let rolled_back = fs.manifest(rollback_receipt.new_generation).unwrap();
    assert_eq!(
        original.entries.get(&path("shared.txt")).unwrap().object_id,
        rolled_back
            .entries
            .get(&path("shared.txt"))
            .unwrap()
            .object_id
    );
    let view = fs.open_branch_view(&branch("main"), 1000, 1000).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(view.view_id, &path("shared.txt"))
            .unwrap(),
        b"initial"
    );
    assert!(fs
        .view_engine()
        .lookup(view.view_id, &path("new.txt"))
        .is_err());
}

#[test]
fn three_branches_have_isolated_uppers_and_heads() {
    let (_root, fs, initial) = initialize();
    let mut branches = Vec::new();
    for name in ["agent/a", "agent/b", "agent/c"] {
        fs.create_branch(RequestId::new(), branch(name), initial.generation_id)
            .unwrap();
        let (_tx, ticket) = begin_ticket(&fs, name);
        fs.view_engine()
            .write_file(ticket.view.view_id, path("shared.txt"), name.as_bytes())
            .unwrap();
        branches.push((name, ticket));
    }
    for (name, ticket) in &branches {
        assert_eq!(
            fs.view_engine()
                .read_file(ticket.view.view_id, &path("shared.txt"))
                .unwrap(),
            name.as_bytes()
        );
    }
    for (name, ticket) in branches {
        let expected = fs.branch_head(&branch(name)).unwrap();
        let candidate = fs
            .prepare_ticket(
                RequestId::new(),
                ticket.ticket.ticket_id,
                Duration::from_secs(1),
            )
            .unwrap();
        fs.publish(
            RequestId::new(),
            candidate.candidate.candidate_id,
            "ok",
            expected,
        )
        .unwrap();
        let stable = fs.open_branch_view(&branch(name), 1000, 1000).unwrap();
        assert_eq!(
            fs.view_engine()
                .read_file(stable.view_id, &path("shared.txt"))
                .unwrap(),
            name.as_bytes()
        );
    }
    assert_eq!(fs.branch_head(&branch("main")).unwrap(), initial);
}

#[test]
fn handles_survive_rename_and_unlink_and_prepare_waits_for_writers() {
    let (_root, fs, _initial) = initialize();
    let (_tx, ticket) = begin_ticket(&fs, "main");
    let view = ticket.view.view_id;
    assert!(fs
        .view_engine()
        .create_file(view, path("shared.txt"), b"x", 0o644, true)
        .is_err());

    let handle = fs
        .view_engine()
        .open_file(view, &path("shared.txt"), true)
        .unwrap();
    fs.view_engine()
        .write_handle(view, handle, 0, b"changed")
        .unwrap();
    let error = fs
        .prepare_ticket(
            RequestId::new(),
            ticket.ticket.ticket_id,
            Duration::from_millis(5),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BusyOpenWriters);
    fs.view_engine()
        .rename(view, path("shared.txt"), path("moved.txt"))
        .unwrap();
    fs.view_engine()
        .write_handle(view, handle, 7, b"!")
        .unwrap();
    assert_eq!(
        fs.view_engine()
            .read_handle(view, handle, &path("shared.txt"), 0, 32)
            .unwrap(),
        b"changed!"
    );
    fs.view_engine().release_file(view, handle).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(view, &path("moved.txt"))
            .unwrap(),
        b"changed!"
    );

    let old = fs
        .view_engine()
        .open_file(view, &path("moved.txt"), false)
        .unwrap();
    fs.view_engine().unlink(view, path("moved.txt")).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_handle(view, old, &path("moved.txt"), 0, 32)
            .unwrap(),
        b"changed!"
    );
    fs.view_engine().release_file(view, old).unwrap();
    assert!(fs.view_engine().lookup(view, &path("moved.txt")).is_err());

    fs.view_engine()
        .create_file(view, path("truncate.txt"), b"abcdef", 0o600, true)
        .unwrap();
    fs.view_engine()
        .truncate(view, path("truncate.txt"), 3)
        .unwrap();
    fs.view_engine()
        .chmod(view, path("truncate.txt"), 0o640)
        .unwrap();
    fs.view_engine()
        .utimens(view, path("truncate.txt"), 122, 123)
        .unwrap();
    let entry = fs
        .view_engine()
        .lookup(view, &path("truncate.txt"))
        .unwrap();
    assert_eq!(entry.size, 3);
    assert_eq!(entry.mode, 0o640);
    assert_eq!(entry.atime_ns, 122);
    assert_eq!(entry.mtime_ns, 123);
}

#[test]
fn concurrent_publish_is_cas_per_branch() {
    let (_root, fs, initial) = initialize();
    let mut candidates = Vec::new();
    for value in [b"one".as_slice(), b"two".as_slice()] {
        let (_tx, ticket) = begin_ticket(&fs, "main");
        fs.view_engine()
            .write_file(ticket.view.view_id, path("shared.txt"), value)
            .unwrap();
        candidates.push(
            fs.prepare_ticket(
                RequestId::new(),
                ticket.ticket.ticket_id,
                Duration::from_secs(1),
            )
            .unwrap()
            .candidate
            .candidate_id,
        );
    }
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = candidates
        .into_iter()
        .map(|candidate| {
            let fs = fs.clone();
            let barrier = Arc::clone(&barrier);
            let expected = initial.clone();
            std::thread::spawn(move || {
                barrier.wait();
                fs.publish(RequestId::new(), candidate, "race", expected)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(error) if error.code == ErrorCode::HeadChanged))
            .count(),
        1
    );
}

#[test]
fn publish_and_abort_are_serialized_per_ticket() {
    let (_root, fs, initial) = initialize();
    let (_tx, ticket) = begin_ticket(&fs, "main");
    fs.view_engine()
        .write_file(ticket.view.view_id, path("shared.txt"), b"race")
        .unwrap();
    let candidate = fs
        .prepare_ticket(
            RequestId::new(),
            ticket.ticket.ticket_id,
            Duration::from_secs(1),
        )
        .unwrap()
        .candidate
        .candidate_id;
    let barrier = Arc::new(Barrier::new(3));
    let publish = {
        let fs = fs.clone();
        let barrier = Arc::clone(&barrier);
        let expected = initial.clone();
        std::thread::spawn(move || {
            barrier.wait();
            fs.publish(RequestId::new(), candidate, "race", expected)
        })
    };
    let abort = {
        let fs = fs.clone();
        let barrier = Arc::clone(&barrier);
        let ticket_id = ticket.ticket.ticket_id;
        std::thread::spawn(move || {
            barrier.wait();
            fs.abort_ticket(RequestId::new(), ticket_id)
        })
    };
    barrier.wait();
    let published = publish.join().unwrap().is_ok();
    let aborted = abort.join().unwrap().is_ok();
    assert_ne!(published, aborted);
    let state = fs.ticket(ticket.ticket.ticket_id).unwrap().state;
    assert_eq!(
        state,
        if published {
            TicketState::Published
        } else {
            TicketState::Aborted
        }
    );
}

#[test]
fn opposing_renames_complete_without_deadlock() {
    let (_root, fs, _initial) = initialize();
    let (_tx, ticket) = begin_ticket(&fs, "main");
    let view = ticket.view.view_id;
    fs.view_engine().mkdir(view, path("left"), 0o755).unwrap();
    fs.view_engine().mkdir(view, path("right"), 0o755).unwrap();
    fs.view_engine()
        .create_file(view, path("left/file"), b"left", 0o644, true)
        .unwrap();
    fs.view_engine()
        .create_file(view, path("right/file"), b"right", 0o644, true)
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let first = {
        let fs = fs.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            fs.view_engine()
                .rename(view, path("left/file"), path("right/file"))
        })
    };
    let second = {
        let fs = fs.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            fs.view_engine()
                .rename(view, path("right/file"), path("left/file"))
        })
    };
    barrier.wait();
    let _ = first.join().unwrap();
    let _ = second.join().unwrap();
    let left = fs.view_engine().lookup(view, &path("left/file")).is_ok();
    let right = fs.view_engine().lookup(view, &path("right/file")).is_ok();
    assert!(left || right);
}

#[test]
fn request_ids_are_idempotent_across_retry_and_restart() {
    let (root, fs, initial) = initialize();
    let begin_tx_request = RequestId::new();
    let tx_a = fs.begin_tx(begin_tx_request, "first").unwrap();
    let tx_b = fs.begin_tx(begin_tx_request, "ignored-on-retry").unwrap();
    assert_eq!(tx_a.tx_id, tx_b.tx_id);
    let reused = fs
        .create_branch(
            begin_tx_request,
            branch("invalid-reuse"),
            initial.generation_id,
        )
        .unwrap_err();
    assert_eq!(reused.code, ErrorCode::AlreadyExistsDifferent);

    let begin_ticket_request = RequestId::new();
    let ticket_a = fs
        .begin_ticket(
            begin_ticket_request,
            tx_a.tx_id,
            branch("main"),
            Some(initial.clone()),
        )
        .unwrap();
    let ticket_b = fs
        .begin_ticket(
            begin_ticket_request,
            tx_a.tx_id,
            branch("main"),
            Some(initial.clone()),
        )
        .unwrap();
    assert_eq!(ticket_a.ticket_id, ticket_b.ticket_id);
    let view_a = fs.open_ticket_view(ticket_a.ticket_id, 1000, 1000).unwrap();
    let view_b = fs.open_ticket_view(ticket_b.ticket_id, 1000, 1000).unwrap();
    assert_ne!(view_a.view_id, view_b.view_id);
    fs.view_engine()
        .write_file(view_a.view_id, path("shared.txt"), b"idempotent")
        .unwrap();

    let prepare_request = RequestId::new();
    let candidate_a = fs
        .prepare_ticket(prepare_request, ticket_a.ticket_id, Duration::from_secs(1))
        .unwrap();
    let candidate_b = fs
        .prepare_ticket(prepare_request, ticket_a.ticket_id, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        candidate_a.candidate.candidate_id,
        candidate_b.candidate.candidate_id
    );
    let publish_request = RequestId::new();
    let receipt_a = fs
        .publish(
            publish_request,
            candidate_a.candidate.candidate_id,
            "same",
            initial.clone(),
        )
        .unwrap();
    let receipt_b = fs
        .publish(
            publish_request,
            candidate_a.candidate.candidate_id,
            "different-ignored",
            initial,
        )
        .unwrap();
    assert_eq!(receipt_a.new_generation, receipt_b.new_generation);
    drop(fs);

    let reopened = CensorFs::open(root.path().join(".censorfs")).unwrap();
    let receipt_c = reopened
        .publish(
            publish_request,
            candidate_a.candidate.candidate_id,
            "still-ignored",
            BranchHead {
                generation_id: receipt_a.old_generation,
                head_seq: receipt_a.head_seq - 1,
            },
        )
        .unwrap();
    assert_eq!(receipt_a.new_generation, receipt_c.new_generation);
    let stored = reopened.request_result(publish_request).unwrap().unwrap();
    assert_eq!(stored.operation, "publish");
}

fn commit_mutation(
    fs: &CensorFs,
    branch_name: &str,
    mutation: impl FnOnce(&censorfs_core::viewfs::ViewEngine, ViewId),
) -> GenerationId {
    let exploration = fs
        .begin_exploration(RequestId::new(), "merge-test", branch(branch_name))
        .unwrap();
    let view = fs
        .open_ticket_view(exploration.ticket.ticket_id, 1000, 1000)
        .unwrap();
    mutation(fs.view_engine(), view.view_id);
    fs.commit_exploration(
        RequestId::new(),
        exploration.ticket.ticket_id,
        "commit",
        Duration::from_secs(1),
    )
    .unwrap()
    .generation
    .generation_id
}

fn commit_path(fs: &CensorFs, branch_name: &str, file: &str, contents: &[u8]) -> GenerationId {
    commit_mutation(fs, branch_name, |engine, view| {
        engine.write_file(view, path(file), contents).unwrap();
    })
}

#[test]
fn three_way_merge_reuses_objects_and_creates_ordered_parents() {
    let (_root, fs, initial) = initialize();
    fs.create_branch(RequestId::new(), branch("source"), initial.generation_id)
        .unwrap();
    fs.create_branch(RequestId::new(), branch("target"), initial.generation_id)
        .unwrap();
    let source_generation = commit_path(&fs, "source", "source.txt", b"source");
    let target_generation = commit_path(&fs, "target", "target.txt", b"target");

    let before = fs.check_merge(branch("source"), branch("target")).unwrap();
    assert!(before.conflicts.is_empty());
    assert_eq!(before.merge_base, initial.generation_id);
    let check_only_target = fs.branch_head(&branch("target")).unwrap();
    assert_eq!(
        fs.check_merge(branch("source"), branch("target"))
            .unwrap()
            .target_head,
        check_only_target
    );

    let request = RequestId::new();
    let merged = fs
        .merge_branches(
            request,
            "merge-test",
            branch("source"),
            branch("target"),
            "merge",
            false,
        )
        .unwrap();
    let receipt = merged.receipt.as_ref().unwrap();
    let meta = fs.generation(receipt.new_generation).unwrap();
    assert_eq!(meta.kind, GenerationKind::Merge);
    assert_eq!(meta.parents, vec![target_generation, source_generation]);
    let source_manifest = fs.manifest(source_generation).unwrap();
    let merge_manifest = fs.manifest(receipt.new_generation).unwrap();
    assert_eq!(
        source_manifest.entries[&path("source.txt")].object_id,
        merge_manifest.entries[&path("source.txt")].object_id
    );
    let view = fs.open_branch_view(&branch("target"), 1000, 1000).unwrap();
    assert_eq!(
        fs.view_engine()
            .read_file(view.view_id, &path("source.txt"))
            .unwrap(),
        b"source"
    );
    assert_eq!(
        fs.view_engine()
            .read_file(view.view_id, &path("target.txt"))
            .unwrap(),
        b"target"
    );

    let retry = fs
        .merge_branches(
            request,
            "ignored",
            branch("source"),
            branch("target"),
            "ignored",
            false,
        )
        .unwrap();
    assert_eq!(
        retry.receipt.unwrap().new_generation,
        receipt.new_generation
    );
}

#[test]
fn merge_conflicts_do_not_create_or_publish_a_generation() {
    let (_root, fs, initial) = initialize();
    fs.create_branch(RequestId::new(), branch("source"), initial.generation_id)
        .unwrap();
    fs.create_branch(RequestId::new(), branch("target"), initial.generation_id)
        .unwrap();
    commit_path(&fs, "source", "shared.txt", b"source");
    commit_path(&fs, "target", "shared.txt", b"target");
    let target_before = fs.branch_head(&branch("target")).unwrap();

    let result = fs
        .merge_branches(
            RequestId::new(),
            "merge-test",
            branch("source"),
            branch("target"),
            "conflict",
            false,
        )
        .unwrap();
    assert_eq!(result.check.conflicts.len(), 1);
    assert_eq!(result.check.conflicts[0].path, path("shared.txt"));
    assert_eq!(
        result.check.conflicts[0].reason,
        MergeConflictReason::BothModified
    );
    assert!(result.prepared.is_none());
    assert!(result.receipt.is_none());
    assert_eq!(fs.branch_head(&branch("target")).unwrap(), target_before);
}

#[test]
fn merge_reports_type_metadata_and_directory_structure_conflicts() {
    let (_root, fs, initial) = initialize();
    for name in [
        "type-source",
        "type-target",
        "meta-source",
        "meta-target",
        "delete-source",
        "delete-target",
    ] {
        fs.create_branch(RequestId::new(), branch(name), initial.generation_id)
            .unwrap();
    }

    commit_mutation(&fs, "type-source", |engine, view| {
        engine.unlink(view, path("shared.txt")).unwrap();
        engine.mkdir(view, path("shared.txt"), 0o755).unwrap();
    });
    commit_path(&fs, "type-target", "shared.txt", b"target");
    let type_conflicts = fs
        .check_merge(branch("type-source"), branch("type-target"))
        .unwrap()
        .conflicts;
    assert!(type_conflicts.iter().any(|conflict| {
        conflict.path == path("shared.txt") && conflict.reason == MergeConflictReason::TypeChanged
    }));

    commit_mutation(&fs, "meta-source", |engine, view| {
        engine.chmod(view, path("shared.txt"), 0o600).unwrap();
    });
    commit_mutation(&fs, "meta-target", |engine, view| {
        engine.chmod(view, path("shared.txt"), 0o640).unwrap();
    });
    let metadata_conflicts = fs
        .check_merge(branch("meta-source"), branch("meta-target"))
        .unwrap()
        .conflicts;
    assert!(metadata_conflicts.iter().any(|conflict| {
        conflict.path == path("shared.txt") && conflict.reason == MergeConflictReason::BothModified
    }));

    commit_mutation(&fs, "delete-source", |engine, view| {
        engine.unlink(view, path("dir/keep.txt")).unwrap();
        engine.rmdir(view, path("dir")).unwrap();
    });
    commit_path(&fs, "delete-target", "dir/keep.txt", b"changed");
    let structural_conflicts = fs
        .check_merge(branch("delete-source"), branch("delete-target"))
        .unwrap()
        .conflicts;
    assert!(structural_conflicts.iter().any(|conflict| {
        conflict.path == path("dir/keep.txt")
            && matches!(
                conflict.reason,
                MergeConflictReason::DeleteModify
                    | MergeConflictReason::ParentMissingOrNotDirectory
            )
    }));
}

#[test]
fn merge_publish_rechecks_the_source_head() {
    let (_root, fs, initial) = initialize();
    fs.create_branch(RequestId::new(), branch("source"), initial.generation_id)
        .unwrap();
    fs.create_branch(RequestId::new(), branch("target"), initial.generation_id)
        .unwrap();
    commit_path(&fs, "source", "source.txt", b"one");
    commit_path(&fs, "target", "target.txt", b"target");
    let check = fs.check_merge(branch("source"), branch("target")).unwrap();
    let target_before = check.target_head.clone();
    let tx = fs.begin_tx(RequestId::new(), "merge-test").unwrap();
    let prepared = fs
        .build_merge_candidate(RequestId::new(), tx.tx_id, check)
        .unwrap();

    commit_path(&fs, "source", "later.txt", b"later");
    let error = fs
        .publish_merge(
            RequestId::new(),
            prepared.candidate.candidate_id,
            "merge",
            prepared.intent,
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::HeadChanged);
    assert_eq!(fs.branch_head(&branch("target")).unwrap(), target_before);
}

#[test]
fn merge_composite_resumes_after_recovery_rebuilds_child_results() {
    let (root, fs, initial) = initialize();
    fs.create_branch(RequestId::new(), branch("source"), initial.generation_id)
        .unwrap();
    fs.create_branch(RequestId::new(), branch("target"), initial.generation_id)
        .unwrap();
    commit_path(&fs, "source", "source.txt", b"source");
    commit_path(&fs, "target", "target.txt", b"target");
    let request_id = RequestId::new();
    let first = fs
        .merge_branches(
            request_id,
            "merge-test",
            branch("source"),
            branch("target"),
            "merge",
            false,
        )
        .unwrap();
    let generation = first.receipt.unwrap().new_generation;
    let storage = root.path().join(".censorfs");
    drop(fs);

    for child in [
        request_id,
        request_id.derive("begin_tx"),
        request_id.derive("prepare_merge"),
        request_id.derive("publish_merge"),
        request_id.derive("close_tx"),
    ] {
        fs::remove_file(storage.join("requests").join(format!("{child}.result"))).unwrap();
    }

    let recovered = CensorFs::open(&storage).unwrap();
    let retried = recovered
        .merge_branches(
            request_id,
            "merge-test",
            branch("source"),
            branch("target"),
            "merge",
            false,
        )
        .unwrap();
    assert_eq!(retried.receipt.unwrap().new_generation, generation);
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry.unwrap();
        let relative = entry.path().strip_prefix(source).unwrap();
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination).unwrap();
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn faulted_flow(fs: &CensorFs) {
    let head = match fs.branch_head(&branch("main")) {
        Ok(value) => value,
        Err(_) => return,
    };
    let dispatcher = ControlDispatcher::new(fs.clone());
    let peer = PeerCredentials {
        pid: 99,
        uid: 1000,
        gid: 1000,
    };
    let opened = dispatcher.dispatch(
        peer,
        request(
            Operation::VariantOpen,
            RequestId::new(),
            0,
            &VariantOpenRequest {
                branch_id: branch("main"),
                expected_head: head.clone(),
                run_id: "fault-run".into(),
                variant_id: "fault-variant".into(),
            },
        )
        .unwrap(),
    );
    if opened.status != 0 {
        return;
    }
    let opened: VariantOpenResult = match decode(&opened.payload) {
        Ok(value) => value,
        Err(_) => return,
    };
    if fs
        .view_engine()
        .write_file(opened.view.view_id, path("shared.txt"), b"faulted")
        .is_err()
    {
        return;
    }
    let prepared = dispatcher.dispatch(
        peer,
        request(
            Operation::VariantPrepare,
            RequestId::new(),
            0,
            &VariantPrepareRequest {
                ticket_id: opened.ticket.ticket_id,
                view_id: Some(opened.view.view_id),
                run_id: opened.run_id.clone(),
                variant_id: opened.variant_id.clone(),
                timeout_ms: 1000,
                max_diff_file_bytes: 64 * 1024,
            },
        )
        .unwrap(),
    );
    if prepared.status != 0 {
        return;
    }
    let prepared: VariantPrepareResult = match decode(&prepared.payload) {
        Ok(value) => value,
        Err(_) => return,
    };
    let _ = dispatcher.dispatch(
        peer,
        request(
            Operation::VariantPublish,
            RequestId::new(),
            0,
            &VariantPublishRequest {
                candidate_id: prepared.candidate.candidate_id,
                expected_head: head,
                decision_id: "fault".into(),
                run_id: opened.run_id,
                variant_id: opened.variant_id,
            },
        )
        .unwrap(),
    );
}

#[test]
fn every_persistence_boundary_recovers_to_a_valid_state() {
    let (baseline, fs, _) = initialize();
    let storage = baseline.path().join(".censorfs");
    drop(fs);

    let count_root = TempDir::new().unwrap();
    let count_storage = count_root.path().join("store");
    copy_tree(&storage, &count_storage);
    let injector = Arc::new(FaultInjectBackend::disabled());
    let instance = CensorFs::open_with_fault(&count_storage, injector.clone()).unwrap();
    injector.reset(usize::MAX);
    faulted_flow(&instance);
    let checkpoints = injector.seen();
    drop(instance);
    assert!(
        checkpoints >= 20,
        "expected all durable layers to expose checkpoints"
    );

    for fail_at in 1..=checkpoints {
        let case = TempDir::new().unwrap();
        let case_storage = case.path().join("store");
        copy_tree(&storage, &case_storage);
        let injector = Arc::new(FaultInjectBackend::disabled());
        let instance = CensorFs::open_with_fault(&case_storage, injector.clone()).unwrap();
        injector.reset(fail_at);
        faulted_flow(&instance);
        drop(instance);

        let recovered = CensorFs::open(&case_storage).unwrap();
        let head = recovered.branch_head(&branch("main")).unwrap();
        recovered.generation(head.generation_id).unwrap();
        for entry in fs::read_dir(recovered.persistence().path("tickets")).unwrap() {
            let path = entry.unwrap().path().join("ticket.meta");
            if !path.exists() {
                continue;
            }
            let ticket: TicketMeta = recovered
                .persistence()
                .read_record(&path, RecordKind::Ticket)
                .unwrap();
            assert!(!matches!(
                ticket.state,
                TicketState::Open | TicketState::Freezing
            ));
        }
        assert_eq!(recovered.superblock().mode, FsMode::ReadWrite);
    }
}
