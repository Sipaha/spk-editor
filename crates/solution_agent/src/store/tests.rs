use super::*;
use crate::adapter::AdapterRegistry;
use crate::model::SessionState;
use crate::test_support::{MockAgentServer, MockConnection};
use chrono::Utc;
use gpui::{SharedString, TestAppContext};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[gpui::test]
fn close_session_removes_from_indices(cx: &mut TestAppContext) {
    let registry = Arc::new(AdapterRegistry::new());
    cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let id = SolutionSessionId::new();
            let entity = cx.new(|_| SolutionSession {
                id,
                solution_id: SolutionId("sol-a".into()),
                agent_id: SharedString::from("claude-acp"),
                acp_session_id: agent_client_protocol::schema::SessionId::new("acp-1"),
                acp_thread: None,
                title: SharedString::from("test"),
                created_at: Utc::now(),
                last_activity_at: Utc::now(),
                state: SessionState::Idle,
                context_count: 1,
                project: None,
                _acp_subscription: None,
                pending_messages: std::collections::VecDeque::new(),
                flush_after_cancel: false,
                cwd: PathBuf::new(),
                cold_entries: Vec::new(),
                last_turn_duration: None,
            });
            store.sessions.insert(id, entity);
            store
                .by_solution
                .entry(SolutionId("sol-a".into()))
                .or_default()
                .push(id);

            assert_eq!(store.sessions_for(&SolutionId("sol-a".into())).len(), 1);
            store.close_session(id, cx).expect("close_session");
            assert_eq!(store.sessions_for(&SolutionId("sol-a".into())).len(), 0);
            assert!(store.session(id).is_none());
        });
    });
}

/// Set up SolutionStore with one Solution rooted at a tempdir, plus
/// a `Project::test` whose worktree is that root. Returns
/// (`SolutionId`, `tempdir`, `Project`). Hold the tempdir for the
/// lifetime of the test — `create_solution` writes to it.
async fn setup_solution_and_project(
    cx: &mut TestAppContext,
) -> (
    SolutionId,
    tempfile::TempDir,
    gpui::Entity<project::Project>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = dir.path().join("solutions.json");
    let solutions_root = dir.path().join("solutions");
    std::fs::create_dir_all(&solutions_root).expect("solutions root");
    let store = cx.update(|cx| {
        let settings_store = settings::SettingsStore::test(cx);
        cx.set_global(settings_store);
        let store = solutions::SolutionStore::for_test(cfg_path, cx);
        solutions::install_global_for_test(store.clone(), cx);
        store
    });
    let solution_id = store
        .update(cx, |store, cx| {
            store.create_solution("Sol", solutions_root.clone(), cx)
        })
        .expect("create_solution");
    let solution_root: PathBuf = store.read_with(cx, |store, _| {
        store
            .solutions()
            .iter()
            .find(|s| s.id == solution_id)
            .map(|s| s.root.clone())
            .expect("solution exists")
    });

    let fs = fs::FakeFs::new(cx.background_executor.clone());
    fs.insert_tree(solution_root.clone(), serde_json::json!({ ".keep": "" }))
        .await;
    let project = project::Project::test(fs, [solution_root.as_path()], cx).await;

    (solution_id, dir, project)
}

#[gpui::test]
async fn pool_release_arms_60s_shutdown_then_drops(cx: &mut TestAppContext) {
    let registry = Arc::new(AdapterRegistry::new());
    cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

    let key = (SolutionId("sol-a".into()), SharedString::from("mock-agent"));

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
            assert_eq!(store.pool_size(), 1);
        });
    });

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.pool_release_session(key.clone(), cx);
        });
    });

    cx.executor()
        .advance_clock(std::time::Duration::from_secs(30));
    cx.executor().run_until_parked();
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| assert_eq!(store.pool_size(), 1));
    });

    cx.executor()
        .advance_clock(std::time::Duration::from_secs(35));
    cx.executor().run_until_parked();
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| assert_eq!(store.pool_size(), 0));
    });
}

#[gpui::test]
async fn shutdown_cancels_when_session_re_added(cx: &mut TestAppContext) {
    let registry = Arc::new(AdapterRegistry::new());
    cx.update(|cx| SolutionAgentStore::init_global(cx, registry));
    let key = (SolutionId("sol-a".into()), SharedString::from("mock-agent"));

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
            store.pool_release_session(key.clone(), cx);
        });
    });

    cx.executor()
        .advance_clock(std::time::Duration::from_secs(30));
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
        });
    });

    cx.executor()
        .advance_clock(std::time::Duration::from_secs(60));
    cx.executor().run_until_parked();
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| assert_eq!(store.pool_size(), 1));
    });
}

#[gpui::test]
async fn create_session_spawns_subprocess_once_per_pair(cx: &mut TestAppContext) {
    let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
    let agent_id = SharedString::from("mock-agent");

    let connect_count = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.register_agent_server(
                agent_id.clone(),
                Rc::new(MockAgentServer::new(connect_count.clone())),
            );
        });
    });

    let session_id = cx
        .update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
            })
        })
        .await
        .expect("create_session");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            assert!(store.session(session_id).is_some());
            assert_eq!(store.pool_size(), 1);
        });
    });
    assert_eq!(connect_count.load(Ordering::SeqCst), 1);
}

#[gpui::test]
async fn parallel_create_session_for_same_pair_spawns_only_once(cx: &mut TestAppContext) {
    let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
    let agent_id = SharedString::from("mock-agent");

    // Gate `connect()` until both create_session calls have observed the
    // pool entry — this guarantees the second call sees `Pending` and
    // doesn't race past into a fresh spawn before the first one inserts.
    let (gate_tx, gate_rx) = async_channel::bounded(1);
    let connect_count = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.register_agent_server(
                agent_id.clone(),
                Rc::new(MockAgentServer::with_gate(connect_count.clone(), gate_rx)),
            );
        });
    });

    let task1 = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
        })
    });
    let task2 = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
        })
    });

    // Pump scheduler so both tasks reach the await on `connect_task`.
    cx.executor().run_until_parked();
    // Now release the gate, letting connect() resolve.
    gate_tx.send(()).await.expect("gate send");
    gate_tx.close();

    let id1 = task1.await.expect("task1");
    let id2 = task2.await.expect("task2");
    assert_ne!(id1, id2);

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            assert_eq!(store.pool_size(), 1);
            assert!(store.session(id1).is_some());
            assert!(store.session(id2).is_some());
        });
    });
    assert_eq!(connect_count.load(Ordering::SeqCst), 1);
}

/// Create a real session (via `create_session`) backed by `MockAgentServer`/
/// `MockConnection`, then return both its id and a clone of the underlying
/// `Entity<AcpThread>` so tests can emit synthetic `AcpThreadEvent`s.
async fn create_session_with_thread(
    cx: &mut TestAppContext,
) -> (
    SolutionSessionId,
    gpui::Entity<acp_thread::AcpThread>,
    tempfile::TempDir,
) {
    let (solution_id, tmp, project) = setup_solution_and_project(cx).await;
    let agent_id = SharedString::from("mock-agent");

    let connect_count = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.register_agent_server(
                agent_id.clone(),
                Rc::new(MockAgentServer::new(connect_count.clone())),
            );
        });
    });

    let session_id = cx
        .update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
            })
        })
        .await
        .expect("create_session");

    let acp_thread = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .acp_thread
            .clone()
            .expect("acp_thread populated")
    });

    (session_id, acp_thread, tmp)
}

#[gpui::test]
async fn turn_complete_event_transitions_running_to_idle(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.state = SessionState::Running {
                    started_at: std::time::Instant::now(),
                    notified: false,
                };
            });
        });
    });

    cx.update(|cx| {
        acp_thread.update(cx, |_thread, cx| {
            cx.emit(acp_thread::AcpThreadEvent::Stopped(
                agent_client_protocol::schema::StopReason::EndTurn,
            ));
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let state = session.read(cx).state.clone();
            assert!(
                matches!(state, SessionState::Idle),
                "expected Idle, got {:?}",
                state
            );
        });
    });
}

#[gpui::test]
async fn error_event_transitions_to_errored_state(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    cx.update(|cx| {
        acp_thread.update(cx, |_thread, cx| {
            cx.emit(acp_thread::AcpThreadEvent::Error);
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let state = session.read(cx).state.clone();
            assert!(
                matches!(state, SessionState::Errored(_)),
                "expected Errored, got {:?}",
                state
            );
        });
    });
}

#[gpui::test]
async fn tool_authorization_request_transitions_to_awaiting_input(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    cx.update(|cx| {
        acp_thread.update(cx, |_thread, cx| {
            cx.emit(acp_thread::AcpThreadEvent::ToolAuthorizationRequested(
                agent_client_protocol::schema::ToolCallId::new("test-tool"),
            ));
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let state = session.read(cx).state.clone();
            assert!(
                matches!(state, SessionState::AwaitingInput),
                "expected AwaitingInput, got {:?}",
                state
            );
        });
    });
}

#[gpui::test]
async fn send_message_starts_running_state_immediately(cx: &mut TestAppContext) {
    let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
    let agent_id = SharedString::from("mock-agent");

    // Use a prompt-gated MockConnection so prompt() stays pending until we
    // release the gate — this lets us observe the synchronous Running flip
    // before the underlying ACP turn completes.
    let (prompt_gate_tx, prompt_gate_rx) = async_channel::bounded::<()>(1);
    let connect_count = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.register_agent_server(
                agent_id.clone(),
                Rc::new(MockAgentServer::with_prompt_gate(
                    connect_count.clone(),
                    prompt_gate_rx,
                )),
            );
        });
    });

    let session_id = cx
        .update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
            })
        })
        .await
        .expect("create_session");

    // Force `Idle` so we can prove `send_message` flips it to `Running`
    // synchronously rather than just observing pre-existing `Running`.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| s.state = SessionState::Idle);
        });
    });

    // Kick off the prompt. We deliberately don't await `task` here — we
    // want to read the state BEFORE the prompt resolves.
    let task = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.send_message(session_id, "hi".into(), cx)
        })
    });

    // Synchronous post-condition: state is already Running.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let state = session.read(cx).state.clone();
            assert!(
                matches!(state, SessionState::Running { .. }),
                "expected Running synchronously after send_message, got {:?}",
                state
            );
        });
    });

    // Now release the prompt gate so the spawned future resolves.
    prompt_gate_tx.send(()).await.expect("release prompt gate");
    prompt_gate_tx.close();
    task.await.expect("send_message task");
}

#[gpui::test]
async fn queued_message_gets_timestamp_marker_on_first_enqueue(cx: &mut TestAppContext) {
    let (session_id, _thread, _tmp) = create_session_with_thread(cx).await;

    // Force `Running` so send_message_blocks takes the queueing branch.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.state = SessionState::Running {
                    started_at: std::time::Instant::now(),
                    notified: false,
                };
            });
            let blocks = vec![agent_client_protocol::schema::ContentBlock::Text(
                agent_client_protocol::schema::TextContent::new("first thought".to_string()),
            )];
            store
                .send_message_blocks(session_id, blocks, cx)
                .detach_and_log_err(cx);
        });
    });

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            assert_eq!(s.pending_messages.len(), 1, "one queued bundle");
            let bundle = &s.pending_messages[0];
            let first = match &bundle[0] {
                agent_client_protocol::schema::ContentBlock::Text(t) => t.text.as_str(),
                other => panic!("first block must be Text, got {other:?}"),
            };
            assert!(
                first.contains("queued in advance"),
                "first block carries the queue marker, got {first:?}"
            );
            assert!(
                first.contains("NOT a direct reply"),
                "marker mentions it's not a direct reply, got {first:?}"
            );
            let payload: String = bundle[1..]
                .iter()
                .filter_map(|b| match b {
                    agent_client_protocol::schema::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            assert!(
                payload.contains("first thought"),
                "user content preserved after marker, got {payload:?}"
            );
        });
    });

    // Second enqueue while still Running should append to the same bundle
    // without injecting a second marker — the queue is one growing message,
    // not a fresh thought, and the marker is set by the first push.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let blocks = vec![agent_client_protocol::schema::ContentBlock::Text(
                agent_client_protocol::schema::TextContent::new("follow-up".to_string()),
            )];
            store
                .send_message_blocks(session_id, blocks, cx)
                .detach_and_log_err(cx);
        });
    });

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            assert_eq!(s.pending_messages.len(), 1, "still one bundle");
            let bundle = &s.pending_messages[0];
            let marker_count = bundle
                .iter()
                .filter(|b| match b {
                    agent_client_protocol::schema::ContentBlock::Text(t) => {
                        t.text.contains("queued in advance")
                    }
                    _ => false,
                })
                .count();
            assert_eq!(marker_count, 1, "marker not duplicated on second enqueue");
            let payload: String = bundle
                .iter()
                .filter_map(|b| match b {
                    agent_client_protocol::schema::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            assert!(
                payload.contains("first thought") && payload.contains("follow-up"),
                "both messages preserved, got {payload:?}"
            );
        });
    });
}

#[gpui::test]
async fn reset_context_swaps_acp_thread_without_bumping_count(cx: &mut TestAppContext) {
    let (session_id, old_thread, _tmp) = create_session_with_thread(cx).await;

    // Snapshot pre-reset state. Bump context_count to 7 so we can prove the
    // reset path does NOT touch it (rotate_context, by contrast, would
    // increment to 8). Also stamp a fake usage onto the old thread so a
    // later "no usage on the new thread" assertion has signal.
    let (old_acp_session_id, old_thread_id) = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| s.context_count = 7);
            let s = session.read(cx);
            (s.acp_session_id.clone(), old_thread.entity_id())
        })
    });

    let result = cx
        .update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.reset_context(session_id, cx))
        })
        .await
        .expect("reset_context");
    assert_eq!(
        result, session_id,
        "reset_context returns the same SolutionSessionId"
    );

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            let new_thread = s.acp_thread.clone().expect("new acp_thread populated");
            assert_ne!(
                new_thread.entity_id(),
                old_thread_id,
                "reset_context swapped the AcpThread entity"
            );
            assert_ne!(
                s.acp_session_id, old_acp_session_id,
                "new acp_session_id differs from the old one"
            );
            assert_eq!(
                s.context_count, 7,
                "context_count unchanged (rotate_context would have bumped to 8)"
            );
            assert!(
                matches!(s.state, SessionState::Idle),
                "state is Idle, got {:?}",
                s.state
            );
            assert!(
                new_thread.read(cx).entries().is_empty(),
                "new thread has no entries"
            );
        });
    });
}

#[gpui::test]
async fn late_send_error_is_dropped_when_session_was_reset(cx: &mut TestAppContext) {
    // Race regression guard: `/clear` (reset_context) swapping the
    // AcpThread mid-turn must not let the OLD turn's late `Err`
    // clobber the freshly-Idle state with `Errored("...")`. Without
    // the `expected_acp_session_id` check in `send_message_blocks`,
    // this test fails — the dropped gate makes the mock prompt return
    // Err, which the spawn's Err branch unconditionally writes as
    // `SessionState::Errored(...)`.
    let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
    let agent_id = SharedString::from("mock-agent");

    let (prompt_gate_tx, prompt_gate_rx) = async_channel::bounded::<()>(1);
    let connect_count = Arc::new(AtomicUsize::new(0));
    cx.update(|cx| {
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            store.register_agent_server(
                agent_id.clone(),
                Rc::new(MockAgentServer::with_prompt_gate(
                    connect_count.clone(),
                    prompt_gate_rx,
                )),
            );
        });
    });

    let session_id = cx
        .update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
            })
        })
        .await
        .expect("create_session");

    // Send a message; the mock will park on the gate.
    let send_task = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            store.send_message(session_id, "hi".into(), cx)
        })
    });

    // Reset the session while the in-flight prompt is still parked. The
    // new ACP thread takes over; state should land on `Idle`.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| store.reset_context(session_id, cx))
    })
    .await
    .expect("reset_context");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            assert!(
                matches!(session.read(cx).state, SessionState::Idle),
                "post-reset state is Idle, got {:?}",
                session.read(cx).state
            );
        });
    });

    // Now release the OLD turn with an error (drop the sender without
    // sending) — without the rotation-race guard, this clobbers Idle
    // with `Errored`.
    prompt_gate_tx.close();
    drop(prompt_gate_tx);
    // The spawned send_task should now resolve to Err. We don't care
    // about its return value; we only care that the side-effect on
    // SessionState was suppressed because the acp_session_id changed.
    let _ = send_task.await;
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            assert!(
                matches!(session.read(cx).state, SessionState::Idle),
                "late Err on rotated session must NOT clobber Idle, got {:?}",
                session.read(cx).state
            );
        });
    });
}

#[gpui::test]
async fn restore_open_tabs_hydrates_cold_sessions(cx: &mut TestAppContext) {
    let (solution_id, _tmp, _project) = setup_solution_and_project(cx).await;
    let registry = Arc::new(AdapterRegistry::new());
    cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

    let executor = cx.executor();
    let db = Arc::new(crate::db::SolutionAgentDb::open(executor).expect("open db"));
    cx.update(|cx| {
        SolutionAgentStore::global(cx).update(cx, |store, _| {
            store.set_persistence(db.clone());
        });
    });

    let id_a = crate::model::SolutionSessionId::new();
    let id_b = crate::model::SolutionSessionId::new();
    let agent_id = SharedString::from("claude-acp");
    let now = Utc::now();

    let meta_a = crate::model::SolutionSessionMetadata {
        id: id_a,
        solution_id: solution_id.clone(),
        agent_id: agent_id.clone(),
        acp_session_id: agent_client_protocol::schema::SessionId::new("acp-a"),
        title: SharedString::from("session A"),
        created_at: now,
        last_activity_at: now,
        preview: None,
        total_tokens: None,
        context_count: 1,
        cwd: PathBuf::new(),
    };
    let meta_b = crate::model::SolutionSessionMetadata {
        id: id_b,
        acp_session_id: agent_client_protocol::schema::SessionId::new("acp-b"),
        title: SharedString::from("session B"),
        ..meta_a.clone()
    };
    db.save_metadata(meta_a).await.expect("meta a");
    db.save_metadata(meta_b).await.expect("meta b");

    let blob_a = serde_json::to_vec(&PersistedSession {
        title: "session A".into(),
        entries: vec![PersistedEntry {
            role: PersistedRole::User,
            markdown: "first prompt".into(),
        }],
        entry_summaries: vec!["first prompt".into()],
    })
    .unwrap();
    db.save_blob(id_a, blob_a).await.expect("blob a");

    db.update_tab_orders(solution_id.clone(), vec![id_b, id_a])
        .await
        .expect("tab order");

    let ordered = cx
        .update(|cx| {
            SolutionAgentStore::global(cx).update(cx, |store, cx| {
                store.restore_open_tabs(solution_id.clone(), cx)
            })
        })
        .await
        .expect("restore");
    assert_eq!(ordered, vec![id_b, id_a]);

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let sa = store.session(id_a).expect("session A restored");
            let sb = store.session(id_b).expect("session B restored");
            sa.read_with(cx, |s, _| {
                assert!(s.is_cold(), "restored session should be cold");
                assert_eq!(s.cold_entries.len(), 1);
                assert!(matches!(s.cold_entries[0].role, PersistedRole::User));
            });
            sb.read_with(cx, |s, _| {
                assert!(s.is_cold());
                // No blob saved for B → cold_entries empty.
                assert!(s.cold_entries.is_empty());
            });
            // sessions_for is what the navigator's reconcile path
            // reads; insertion order into `by_solution` must match
            // the `tab_order ASC` returned by the DB so the strip
            // ends up identical to what the user closed last time.
            let listed: Vec<_> = store
                .sessions_for(&solution_id)
                .into_iter()
                .map(|entity| entity.read(cx).id)
                .collect();
            assert_eq!(listed, vec![id_b, id_a]);
        });
    });
}

#[test]
fn persisted_session_roundtrips_with_structured_entries() {
    let original = PersistedSession {
        title: "demo".into(),
        entries: vec![
            PersistedEntry {
                role: PersistedRole::User,
                markdown: "Hello".into(),
            },
            PersistedEntry {
                role: PersistedRole::Assistant,
                markdown: "Hi there!".into(),
            },
            PersistedEntry {
                role: PersistedRole::Tool,
                markdown: "ran tool x".into(),
            },
        ],
        entry_summaries: vec!["Hello".into(), "Hi there!".into(), "ran tool x".into()],
    };
    let bytes = serde_json::to_vec(&original).unwrap();
    let decoded: PersistedSession = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.title, original.title);
    assert_eq!(decoded.entries.len(), 3);
    assert!(matches!(decoded.entries[0].role, PersistedRole::User));
    assert!(matches!(decoded.entries[1].role, PersistedRole::Assistant));
    assert!(matches!(decoded.entries[2].role, PersistedRole::Tool));
    assert_eq!(decoded.entries[0].markdown, "Hello");
    assert_eq!(decoded.entry_summaries.len(), 3);
}

#[test]
fn persisted_session_legacy_blob_decodes_with_empty_entries() {
    let legacy_json = serde_json::json!({
        "title": "old session",
        "entry_summaries": ["one", "two"],
    });
    let bytes = serde_json::to_vec(&legacy_json).unwrap();
    let decoded: PersistedSession = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.title, "old session");
    assert!(
        decoded.entries.is_empty(),
        "legacy blobs have no entries field"
    );
    assert_eq!(
        decoded.entry_summaries,
        vec!["one".to_string(), "two".to_string()]
    );
}
