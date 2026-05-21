use super::*;
use crate::adapter::AdapterRegistry;
use crate::model::SessionState;
use crate::test_support::{MockAgentServer, MockConnection};
use chrono::Utc;
use gpui::{Entity, SharedString, TestAppContext};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Insert a minimal cold session (no `acp_thread`) directly into the store
/// for tests that need a pre-existing session without going through the full
/// `create_session` → ACP-handshake flow.
pub(crate) fn insert_cold_session(
    session_id: crate::model::SolutionSessionId,
    solution_id: solutions::SolutionId,
    agent_id: gpui::SharedString,
    cached_total_tokens: Option<u64>,
    project: Option<Entity<project::Project>>,
    store: &mut SolutionAgentStore,
    cx: &mut gpui::Context<SolutionAgentStore>,
) -> Entity<crate::model::SolutionSession> {
    let session = cx.new(|_| {
        let mut s = crate::model::SolutionSession::new_idle(
            session_id,
            solution_id.clone(),
            agent_id,
            agent_client_protocol::schema::SessionId::new("acp-cold"),
        );
        s.title = SharedString::from("Cold");
        s.project = project;
        s.cached_total_tokens = cached_total_tokens;
        s
    });
    store.sessions.insert(session_id, session.clone());
    store
        .by_solution
        .entry(solution_id)
        .or_default()
        .push(session_id);
    session
}

#[gpui::test]
fn close_session_removes_from_indices(cx: &mut TestAppContext) {
    let registry = Arc::new(AdapterRegistry::new());
    cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let id = SolutionSessionId::new();
            let entity = cx.new(|_| {
                let mut s = SolutionSession::new_idle(
                    id,
                    SolutionId("sol-a".into()),
                    SharedString::from("claude-acp"),
                    agent_client_protocol::schema::SessionId::new("acp-1"),
                );
                s.title = SharedString::from("test");
                s
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
pub(crate) async fn setup_solution_and_project(
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
pub(crate) async fn create_session_with_thread(
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
            .acp_thread()
            .cloned()
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
            let new_thread = s.acp_thread().cloned().expect("new acp_thread populated");
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
async fn reset_context_clears_cold_entries(cx: &mut TestAppContext) {
    let (session_id, _old_thread, _tmp) = create_session_with_thread(cx).await;

    // Stamp a fake cold entry so we can prove `reset_context` clears it.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.cold_entries
                    .push(acp_thread::AgentThreadEntry::AssistantMessage(
                        acp_thread::AssistantMessage {
                            chunks: Vec::new(),
                            indented: false,
                            is_subagent_output: false,
                        },
                    ));
            });
            cx.notify();
        });
    });

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
            let s = session.read(cx);
            assert!(
                s.cold_entries.is_empty(),
                "reset_context should drop cold_entries (was {:?})",
                s.cold_entries.len()
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
        parent_session_id: None,
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
        entries_v2: vec![],
        entry_created_ms: vec![],
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
                // v1 blobs hydrate as Assistant-shaped legacy rows
                // (the old `role` field is no longer round-tripped —
                // structured v2 carries the real role per variant).
                assert!(matches!(
                    s.cold_entries[0],
                    acp_thread::AgentThreadEntry::AssistantMessage(_)
                ));
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
        entries_v2: vec![],
        entry_created_ms: vec![],
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

/// `EntriesRemoved` covers thread-local truncation; the `cleared` arm
/// fires when `entries()` is empty after the event (the only in-tree
/// producer is rewind-to-zero from refusal-truncation). This test pins
/// that the live thread's `token_usage` and the session's
/// `cached_total_tokens`/`last_turn_duration` mirrors all reset on the
/// rewind-to-zero path; the partial-rewind sibling
/// (`entries_removed_partial_rewind_preserves_token_state`) pins the
/// negative case. The user-facing `/clear` flow is covered by
/// `reset_context_resets_token_meter`.
#[gpui::test]
async fn entries_removed_to_zero_resets_token_state(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Stamp pre-clear state on both the live thread (token_usage) and
    // the session (cached_total_tokens, last_turn_duration). All three
    // must be cleared on full /clear.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.update_token_usage(
                Some(acp_thread::TokenUsage {
                    used_tokens: 12_345,
                    max_tokens: 1_000_000,
                    ..Default::default()
                }),
                cx,
            );
        });
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.cached_total_tokens = Some(12_345);
                s.last_turn_duration = Some(std::time::Duration::from_secs(7));
            });
        });
    });

    // Emit EntriesRemoved. Range payload is informational; the handler
    // discriminates full clear vs partial rewind by checking
    // `entries().is_empty()` post-event. The mock thread starts empty
    // and we never appended, so this exercises the cleared-arm.
    cx.update(|cx| {
        acp_thread.update(cx, |_t, cx| {
            cx.emit(acp_thread::AcpThreadEvent::EntriesRemoved(0..0));
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            assert!(
                s.cached_total_tokens.is_none(),
                "cached_total_tokens reset, got {:?}",
                s.cached_total_tokens
            );
            assert!(
                s.last_turn_duration.is_none(),
                "last_turn_duration reset, got {:?}",
                s.last_turn_duration
            );
        });
        let usage = acp_thread.read(cx).token_usage().cloned();
        assert!(
            usage.is_none(),
            "live thread token_usage reset, got {usage:?}"
        );
    });
}

/// Sibling of `entries_removed_full_clear_resets_token_state`: when
/// `EntriesRemoved` fires but the live thread still has surviving
/// entries (a `rewind` to a specific user message rather than a full
/// `/clear`), the agent will emit a fresh `TokenUsageUpdated` reflecting
/// the surviving prefix's usage — so we must NOT preemptively wipe
/// token state. This pins the partial-rewind branch; the existence of
/// this test plus the full-clear sibling means a future "always reset
/// on EntriesRemoved" mutation breaks one of them.
#[gpui::test]
async fn entries_removed_partial_rewind_preserves_token_state(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Seed the live thread with a surviving user entry so the handler's
    // `entries().is_empty()` discriminator returns false.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                "survivor".into(),
                cx,
            );
            t.update_token_usage(
                Some(acp_thread::TokenUsage {
                    used_tokens: 9_999,
                    max_tokens: 1_000_000,
                    ..Default::default()
                }),
                cx,
            );
        });
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.cached_total_tokens = Some(9_999);
                s.last_turn_duration = Some(std::time::Duration::from_secs(11));
            });
        });
    });

    // Emit EntriesRemoved over an arbitrary range — what discriminates
    // partial-rewind from full-clear is `entries().is_empty()` on the
    // live thread, not the event payload. With one surviving entry,
    // the cleared arm must be skipped.
    cx.update(|cx| {
        acp_thread.update(cx, |_t, cx| {
            cx.emit(acp_thread::AcpThreadEvent::EntriesRemoved(0..1));
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            assert_eq!(
                s.cached_total_tokens,
                Some(9_999),
                "cached_total_tokens preserved on partial rewind",
            );
            assert_eq!(
                s.last_turn_duration,
                Some(std::time::Duration::from_secs(11)),
                "last_turn_duration preserved on partial rewind",
            );
        });
        let usage = acp_thread.read(cx).token_usage().cloned();
        assert!(
            usage.is_some_and(|u| u.used_tokens == 9_999),
            "live thread token_usage preserved on partial rewind",
        );
    });
}

/// User-facing `/clear` is intercepted client-side and routed through
/// `reset_context`, which spawns a brand-new `AcpThread` (the old one
/// is dropped without emitting any events). Without an explicit reset
/// at the swap site, `cached_total_tokens` / `last_turn_duration` on
/// the session entity persist across the swap and the status-row meter
/// keeps reading the pre-clear count (because the meter falls back to
/// `cached_total_tokens` when the live thread has no `token_usage`,
/// which it doesn't on a fresh thread). This test pins the reset at
/// the swap site — the actual user-visible bug.
#[gpui::test]
async fn reset_context_resets_token_meter(cx: &mut TestAppContext) {
    let (session_id, _old_thread, _tmp) = create_session_with_thread(cx).await;

    // Stamp pre-clear cached values on the session.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.cached_total_tokens = Some(33_333);
                s.last_turn_duration = Some(std::time::Duration::from_secs(13));
            });
        });
    });

    // Drive the actual `/clear` flow.
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
            let s = session.read(cx);
            assert!(
                s.cached_total_tokens.is_none(),
                "cached_total_tokens reset, got {:?}",
                s.cached_total_tokens
            );
            assert!(
                s.last_turn_duration.is_none(),
                "last_turn_duration reset, got {:?}",
                s.last_turn_duration
            );
            // Sanity: live thread is fresh and has no usage.
            let new_thread = s.acp_thread().cloned().expect("new thread populated");
            assert!(
                new_thread.read(cx).token_usage().is_none(),
                "fresh thread has no token_usage"
            );
        });
    });
}

/// Same invariant for `/compact` (rotate_context) — same swap pattern,
/// same risk of stale meter.
#[gpui::test]
async fn rotate_context_resets_token_meter(cx: &mut TestAppContext) {
    let (session_id, _old_thread, _tmp) = create_session_with_thread(cx).await;

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            session.update(cx, |s, _| {
                s.cached_total_tokens = Some(44_444);
                s.last_turn_duration = Some(std::time::Duration::from_secs(17));
            });
        });
    });

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| store.rotate_context(session_id, cx))
    })
    .await
    .expect("rotate_context");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let s = session.read(cx);
            assert!(
                s.cached_total_tokens.is_none(),
                "cached_total_tokens reset on rotate, got {:?}",
                s.cached_total_tokens
            );
            assert!(
                s.last_turn_duration.is_none(),
                "last_turn_duration reset on rotate, got {:?}",
                s.last_turn_duration
            );
        });
    });
}

/// `build_session_meta` shapes the system prompt into the exact JSON
/// envelope claude-agent-acp expects: `{ "systemPrompt": { "append": "<text>" } }`.
/// A wrong key name or nesting level silently drops the prompt — the
/// agent ignores unknown `_meta` keys per the ACP spec, so a typo here
/// would not surface as an error and the bug would only manifest as
/// "agent has no idea it's in a Solution". Pin the shape AND the empty-
/// prompt None path so future adapter changes can't regress either.
#[gpui::test]
fn build_session_meta_emits_correct_json_shape(cx: &mut TestAppContext) {
    use crate::claude_adapter::{CLAUDE_ACP_AGENT_ID, ClaudeAcpAdapter};
    use solutions::{CatalogId, Solution, SolutionMember};

    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ClaudeAcpAdapter));
    cx.update(|cx| SolutionAgentStore::init_global(cx, Arc::new(registry)));

    let solution = Solution {
        id: SolutionId("sol-meta".into()),
        name: "test-meta".into(),
        root: PathBuf::from("/tmp/sol-meta"),
        members: vec![SolutionMember {
            catalog_id: CatalogId("cat-foo".into()),
            local_path: PathBuf::from("/tmp/sol-meta/foo"),
        }],
        last_opened_at: Some(Utc::now()),
    };

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            let meta = store
                .build_session_meta(&SharedString::from(CLAUDE_ACP_AGENT_ID), &solution)
                .expect("registered ClaudeAcpAdapter produces a non-empty prompt");
            let system_prompt = meta
                .get("systemPrompt")
                .expect("meta carries `systemPrompt` key (camelCase, not snake_case — claude-agent-acp matches exactly)")
                .as_object()
                .expect("`systemPrompt` is an object (not a bare string — agent reads `append` field)");
            let append = system_prompt
                .get("append")
                .expect("`append` key present (vs. replacing the preset entirely)")
                .as_str()
                .expect("`append` value is a string");
            assert!(
                append.contains("/tmp/sol-meta"),
                "prompt mentions solution root, got {append:?}"
            );
            assert!(
                append.contains("foo"),
                "prompt mentions member project, got {append:?}"
            );

            // Unknown agent → None (registry lookup fails)
            let none_meta = store
                .build_session_meta(&SharedString::from("not-registered"), &solution);
            assert!(none_meta.is_none(), "unknown agent yields None");
        });
    });

    // Empty-prompt branch: a registered adapter that produces an empty
    // string must yield None so we don't ship a `_meta.systemPrompt:
    // {append: ""}` envelope (claude-agent-acp would then append nothing
    // to the preset and the round-trip wastes bandwidth + clutters the
    // request log).
    struct EmptyAdapter;
    impl crate::adapter::SolutionAgentAdapter for EmptyAdapter {
        fn agent_id(&self) -> AgentServerId {
            SharedString::from("empty-adapter")
        }
        fn display_name(&self) -> SharedString {
            SharedString::from("empty")
        }
        fn icon(&self) -> ui::IconName {
            ui::IconName::Sparkle
        }
        fn build_initial_system_prompt(&self, _: &Solution) -> String {
            String::new()
        }
    }
    cx.update(|cx| {
        let mut empty_registry = AdapterRegistry::new();
        empty_registry.register(Arc::new(EmptyAdapter));
        SolutionAgentStore::init_global(cx, Arc::new(empty_registry));
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, _| {
            let meta = store.build_session_meta(&SharedString::from("empty-adapter"), &solution);
            assert!(meta.is_none(), "empty prompt yields None");
        });
    });
}

// =====================================================================
// Auto-wake for MCP `send_message_blocks` on cold sessions
// =====================================================================

/// Sending to a cold session whose owning Solution is gone from
/// `SolutionStore` returns the structured `unknown_solution` error —
/// not the legacy "session has no ACP thread yet" — so MCP clients can
/// distinguish "the agent isn't running yet (we'll wake it)" from
/// "this session is orphaned (give up)".
#[gpui::test]
async fn cold_send_unknown_solution_returns_structured_error(cx: &mut TestAppContext) {
    // Use a SolutionId that won't be in SolutionStore. We still need
    // SolutionStore initialised because `SolutionAgentStore::init_global`
    // subscribes to it.
    let dir = tempfile::tempdir().expect("tempdir");
    cx.update(|cx| {
        let settings_store = settings::SettingsStore::test(cx);
        cx.set_global(settings_store);
        let solutions_store =
            solutions::SolutionStore::for_test(dir.path().join("solutions.json"), cx);
        solutions::install_global_for_test(solutions_store, cx);
        let registry = Arc::new(AdapterRegistry::new());
        SolutionAgentStore::init_global(cx, registry);
    });

    let orphan_solution_id = SolutionId("orphan-sol".into());
    let session_id = SolutionSessionId::new();
    let agent_id = SharedString::from("mock-agent");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            insert_cold_session(
                session_id,
                orphan_solution_id.clone(),
                agent_id.clone(),
                None,
                None,
                store,
                cx,
            );
        });
    });

    let task = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let blocks = vec![agent_client_protocol::schema::ContentBlock::Text(
                agent_client_protocol::schema::TextContent::new("hello".to_string()),
            )];
            store.send_message_blocks(session_id, blocks, cx)
        })
    });

    let err = task.await.expect_err("orphan solution must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown_solution"),
        "expected structured 'unknown_solution' error, got {msg:?}"
    );
    assert!(
        !msg.contains("has no ACP thread yet"),
        "auto-wake should replace the legacy 'no ACP thread' error, got {msg:?}"
    );
}

/// Hot-path passthrough: when a session has a live `acp_thread`,
/// `send_message_blocks` flips the state to Running synchronously
/// without entering the wake path — the wake helper must not interfere
/// with already-attached sessions.
#[gpui::test]
async fn hot_send_does_not_enter_wake_path(cx: &mut TestAppContext) {
    let (session_id, _thread, _tmp) = create_session_with_thread(cx).await;

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let blocks = vec![agent_client_protocol::schema::ContentBlock::Text(
                agent_client_protocol::schema::TextContent::new("hot path".to_string()),
            )];
            // Detach — we only care that the synchronous state flip
            // happened. The actual prompt path uses the MockConnection
            // which returns Err without a gate (see test_support); that
            // would arrive as `Errored` after the spawn resolves.
            store
                .send_message_blocks(session_id, blocks, cx)
                .detach_and_log_err(cx);
        });
    });

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| {
            let session = store.session(session_id).expect("session exists");
            let state = session.read(cx).state.clone();
            assert!(
                matches!(state, SessionState::Running { .. }),
                "hot path should flip to Running synchronously, got {state:?}"
            );
        });
    });
}

#[gpui::test]
async fn append_stamps_entry_created_ms_once_per_index(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append a user entry. `push_user_content_block` creates a new
    // UserMessage entry (no existing user message last), so `push_entry`
    // fires, which emits `AcpThreadEvent::NewEntry`.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hello".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    // Append an assistant entry. The thread's last entry is now UserMessage,
    // so `push_assistant_content_block` also calls `push_entry` → `NewEntry`.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_assistant_content_block(
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("world".to_string()),
                ),
                false,
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let stamps = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.read(cx).session(session_id).expect("session exists").read(cx).entry_created_ms.clone()
    });
    assert_eq!(stamps.len(), 2, "two appends → two timestamps");
    assert!(stamps[1] >= stamps[0], "timestamps are non-decreasing");

    // Now drive an in-place EntryUpdated on the last entry (streaming more
    // text into the existing assistant message). `push_assistant_content_block`
    // with an existing assistant entry as the last entry emits `EntryUpdated`,
    // NOT `NewEntry` — so the vector must NOT grow or change.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_assistant_content_block(
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new(" more text".to_string()),
                ),
                false,
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let after = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.read(cx).session(session_id).expect("session exists").read(cx).entry_created_ms.clone()
    });
    assert_eq!(after.len(), 2, "in-place update must not add a timestamp");
    assert_eq!(after[1], stamps[1], "existing timestamp must be unchanged");
}

#[gpui::test]
async fn append_after_resumed_unstamped_history_does_not_fabricate(cx: &mut TestAppContext) {
    use crate::model::NO_TIMESTAMP_MS;

    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append two entries (user + assistant). These get real stamps on the
    // normal path.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hello".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_assistant_content_block(
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("world".to_string()),
                ),
                false,
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    // Simulate a resumed pre-feature session: the legacy blob carried no
    // timestamps, so the restore path leaves `entry_created_ms` empty even
    // though the live thread already holds historical entries. Force that
    // state directly — empty vector under a populated (2-entry) thread.
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        let session = store.read(cx).session(session_id).expect("session exists");
        session.update(cx, |s, _| s.entry_created_ms.clear());
    });

    // Now the user sends a new message → a genuinely-new entry arrives at
    // `entry_index == 2` (the thread already has 2 historical entries).
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("new".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let stamps = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .clone()
    });

    // The vector must stay 1:1 with the thread's 3 entries.
    assert_eq!(stamps.len(), 3, "gap-fill must keep the vector index-aligned");
    // The two historical (gap-filled) indices must NOT be fabricated.
    assert_eq!(
        stamps[0], NO_TIMESTAMP_MS,
        "historical gap entry must be marked absent, not fabricated"
    );
    assert_eq!(
        stamps[1], NO_TIMESTAMP_MS,
        "historical gap entry must be marked absent, not fabricated"
    );
    // Only the just-appended entry gets a real positive timestamp.
    assert!(
        stamps[2] > 0,
        "the genuinely-new entry must hold a real positive timestamp, got {}",
        stamps[2]
    );
}

#[gpui::test]
async fn entry_created_ms_survives_persist_roundtrip(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append two entries (user + assistant) so the hot `entry_created_ms`
    // gets two stamps, index-aligned with the live thread entries.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hello".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_assistant_content_block(
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("world".to_string()),
                ),
                false,
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    // (a) Roundtrip: produce the real persisted blob via the same path the
    // store writes, decode it, and assert the timestamps survive intact and
    // stay index-aligned with the persisted entries.
    let (original_stamps, decoded) = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        let session = store.read(cx).session(session_id).expect("session exists");
        let session = session.read(cx);
        let original_stamps = session.entry_created_ms.clone();
        let bytes = serializable_snapshot(session, cx);
        let decoded: PersistedSession = serde_json::from_slice(&bytes).unwrap();
        (original_stamps, decoded)
    });
    assert_eq!(original_stamps.len(), 2, "two appends → two hot stamps");
    assert_eq!(
        decoded.entry_created_ms, original_stamps,
        "persisted timestamps must roundtrip unchanged"
    );
    assert_eq!(
        decoded.entry_created_ms.len(),
        decoded.entries_v2.len(),
        "timestamp vector must stay index-aligned with entries_v2"
    );

    // (b) Absent sentinel roundtrips: force the first hot stamp to
    // NO_TIMESTAMP_MS (an entry whose creation time was never captured) and
    // confirm `serializable_snapshot` + serde preserves it rather than turning
    // it into 0 or dropping it, and that the vector stays index-aligned.
    use crate::model::NO_TIMESTAMP_MS;
    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        let session = store.read(cx).session(session_id).expect("session exists");
        session.update(cx, |s, _| s.entry_created_ms[0] = NO_TIMESTAMP_MS);
    });
    let decoded_sentinel = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        let session = store.read(cx).session(session_id).expect("session exists");
        let session = session.read(cx);
        let bytes = serializable_snapshot(session, cx);
        serde_json::from_slice::<PersistedSession>(&bytes).unwrap()
    });
    assert_eq!(
        decoded_sentinel.entry_created_ms[0], NO_TIMESTAMP_MS,
        "absent sentinel must survive persist roundtrip, not become 0 or be dropped"
    );
    assert_eq!(
        decoded_sentinel.entry_created_ms.len(),
        decoded_sentinel.entries_v2.len(),
        "sentinel-bearing vector must stay index-aligned with entries_v2"
    );

    // (c) Legacy decode: a blob without the `entry_created_ms` key decodes to
    // an empty vec (proves `#[serde(default)]`).
    let legacy = serde_json::json!({
        "title": "t",
        "entry_summaries": [],
        "entries_v2": []
    });
    let decoded: PersistedSession = serde_json::from_value(legacy).unwrap();
    assert!(decoded.entry_created_ms.is_empty());
}

#[gpui::test]
async fn reset_context_clears_entry_created_ms(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append one user entry so entry_created_ms is non-empty.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hello".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let len_before = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .len()
    });
    assert_eq!(len_before, 1, "one append → one timestamp before reset");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| store.reset_context(session_id, cx))
    })
    .await
    .expect("reset_context");

    let len_after = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .len()
    });
    assert_eq!(
        len_after,
        0,
        "reset_context clears the timestamp vector with the entries"
    );
}

/// `EntriesRemoved` must truncate `entry_created_ms` at `range.start`,
/// keeping the surviving prefix aligned with the surviving thread entries.
/// This exercises the actual truncation path on a populated vector;
/// `entries_removed_partial_rewind_preserves_token_state` covers the token
/// state side independently.
#[gpui::test]
async fn entries_removed_truncates_entry_created_ms(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append two entries so entry_created_ms.len() == 2.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("first".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_assistant_content_block(
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("second".to_string()),
                ),
                false,
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let stamps = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .clone()
    });
    assert_eq!(stamps.len(), 2, "two appends → two timestamps before removal");

    // Emit EntriesRemoved(1..2) — removes the last entry. The live thread
    // still has one surviving entry (the user message), so this is a
    // partial rewind: the handler truncates entry_created_ms to length 1
    // but does NOT reset token state.
    cx.update(|cx| {
        acp_thread.update(cx, |_t, cx| {
            cx.emit(acp_thread::AcpThreadEvent::EntriesRemoved(1..2));
        });
    });
    cx.executor().run_until_parked();

    let after = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .clone()
    });
    assert_eq!(
        after.len(),
        1,
        "EntriesRemoved(1..2) must truncate entry_created_ms to length 1"
    );
    assert_eq!(
        after[0], stamps[0],
        "surviving stamp at index 0 must be unchanged"
    );
}

/// `rotate_context` swaps the underlying ACP thread and clears
/// `entry_created_ms`. Without this, timestamps from the old thread
/// would bleed into the new context.
#[gpui::test]
async fn rotate_context_clears_entry_created_ms(cx: &mut TestAppContext) {
    let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

    // Append one user entry so entry_created_ms is non-empty before rotation.
    cx.update(|cx| {
        acp_thread.update(cx, |t, cx| {
            t.push_user_content_block(
                Some(acp_thread::UserMessageId::new()),
                agent_client_protocol::schema::ContentBlock::Text(
                    agent_client_protocol::schema::TextContent::new("hello".to_string()),
                ),
                cx,
            );
        });
    });
    cx.executor().run_until_parked();

    let len_before = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .len()
    });
    assert_eq!(len_before, 1, "one append → one timestamp before rotation");

    cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| store.rotate_context(session_id, cx))
    })
    .await
    .expect("rotate_context");

    let len_after = cx.update(|cx| {
        let store = SolutionAgentStore::global(cx);
        store
            .read(cx)
            .session(session_id)
            .expect("session exists")
            .read(cx)
            .entry_created_ms
            .len()
    });
    assert_eq!(
        len_after,
        0,
        "rotate_context clears the timestamp vector with the entries"
    );
}
