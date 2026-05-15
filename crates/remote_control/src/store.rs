use std::sync::Arc;

use anyhow::{Result, anyhow};
use base64::Engine as _;
use chrono::Utc;
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use rand::TryRngCore as _;
use rand::rngs::OsRng;

use crate::model::{AuthorizedClient, RemoteControlSettings};
use crate::settings;

/// Number of bytes of OS randomness per client secret. 32 bytes → 256 bits
/// of entropy, sized for use as a session key by the future Remote Control
/// listener. Base64-encoded → 44 chars (32 / 3 * 4, padded).
pub const CLIENT_SECRET_BYTES: usize = 32;

pub struct RemoteControlStore {
    settings: RemoteControlSettings,
    fs: Option<Arc<dyn fs::Fs>>,
    /// Live FS watcher task (dropped → watcher stops).
    _watcher: Option<Task<()>>,
}

#[derive(Clone, Debug)]
pub enum RemoteControlStoreEvent {
    /// Any persisted-state mutation. The UI re-renders on this event.
    Changed,
}

impl EventEmitter<RemoteControlStoreEvent> for RemoteControlStore {}

struct GlobalRemoteControlStore(Entity<RemoteControlStore>);
impl Global for GlobalRemoteControlStore {}

impl RemoteControlStore {
    fn empty() -> Self {
        Self {
            settings: RemoteControlSettings::default(),
            fs: None,
            _watcher: None,
        }
    }

    /// Build the global store and start watching the on-disk JSON file. The
    /// global `dyn Fs` must already be installed (it is — see
    /// `crates/zed/src/main.rs`, which sets the FS global well before
    /// `remote_control::init` runs). In tests, instantiate the store via
    /// `RemoteControlStore::new_with_fs` instead of going through `init_global`.
    pub fn init_global(cx: &mut App) {
        let fs = <dyn fs::Fs>::global(cx);
        let store = cx.new(|cx| Self::new_with_fs(fs, cx));
        cx.set_global(GlobalRemoteControlStore(store));
    }

    /// Build a store backed by `fs`, starting its watcher.
    pub fn new_with_fs(fs: Arc<dyn fs::Fs>, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty();
        this.fs = Some(fs);
        this.spawn_watch(cx);
        this
    }

    pub fn global(cx: &App) -> Entity<RemoteControlStore> {
        cx.global::<GlobalRemoteControlStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<RemoteControlStore>> {
        cx.try_global::<GlobalRemoteControlStore>().map(|g| g.0.clone())
    }

    pub fn settings(&self) -> &RemoteControlSettings {
        &self.settings
    }

    /// Replace the entire settings struct. Emits `Changed` and persists to
    /// disk (best-effort).
    pub fn update_settings(
        &mut self,
        settings: RemoteControlSettings,
        cx: &mut Context<Self>,
    ) {
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        self.notify_changed(cx);
        self.save_to_disk(cx).detach();
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.enabled == enabled {
            return;
        }
        self.settings.enabled = enabled;
        self.notify_changed(cx);
        self.save_to_disk(cx).detach();
    }

    pub fn set_address(&mut self, address: Option<String>, cx: &mut Context<Self>) {
        let normalized = address.and_then(|raw| {
            let trimmed = raw.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        if self.settings.server_address == normalized {
            return;
        }
        self.settings.server_address = normalized;
        self.notify_changed(cx);
        self.save_to_disk(cx).detach();
    }

    pub fn set_port(&mut self, port: u16, cx: &mut Context<Self>) {
        if self.settings.server_port == port {
            return;
        }
        self.settings.server_port = port;
        self.notify_changed(cx);
        self.save_to_disk(cx).detach();
    }

    /// Add a new authorized client with the given name. Returns the freshly
    /// constructed client (so the UI can show its secret prefix immediately).
    /// Fails when a client with the same name already exists, or when the
    /// platform's OS RNG isn't available.
    pub fn add_client(
        &mut self,
        name: String,
        cx: &mut Context<Self>,
    ) -> Result<AuthorizedClient> {
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow!("client name cannot be empty"));
        }
        if self
            .settings
            .clients
            .iter()
            .any(|existing| existing.name == trimmed)
        {
            return Err(anyhow!("client {trimmed:?} already exists"));
        }
        let secret_base64 = generate_secret()?;
        let client = AuthorizedClient {
            name: trimmed,
            secret_base64,
            created_at: Utc::now(),
        };
        self.settings.clients.push(client.clone());
        self.notify_changed(cx);
        self.save_to_disk(cx).detach();
        Ok(client)
    }

    pub fn remove_client(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let before = self.settings.clients.len();
        self.settings.clients.retain(|client| client.name != name);
        let removed = self.settings.clients.len() < before;
        if removed {
            self.notify_changed(cx);
            self.save_to_disk(cx).detach();
        }
        removed
    }

    fn notify_changed(&self, cx: &mut Context<Self>) {
        cx.emit(RemoteControlStoreEvent::Changed);
        cx.notify();
    }

    fn save_to_disk(&self, cx: &App) -> Task<()> {
        let Some(fs) = self.fs.clone() else {
            log::warn!("remote_control: no fs registered, skipping persistence");
            return Task::ready(());
        };
        let path = paths::remote_control_settings_file().clone();
        let text = settings::render(&self.settings);
        cx.background_spawn(async move {
            if let Some(parent) = path.parent() {
                if let Err(err) = fs.create_dir(parent).await {
                    log::warn!("remote_control: creating {parent:?}: {err:#}");
                    return;
                }
            }
            if let Err(err) = fs.atomic_write(path.clone(), text).await {
                log::warn!("remote_control: writing {path:?}: {err:#}");
            }
        })
    }

    fn spawn_watch(&mut self, cx: &mut Context<Self>) {
        let Some(fs) = self.fs.clone() else {
            return;
        };
        let path = paths::remote_control_settings_file().clone();
        let task = cx.spawn(async move |this, cx| {
            let (mut contents_rx, _watcher) =
                ::settings::watch_config_file(cx.background_executor(), fs, path);
            while let Some(text) = contents_rx.next().await {
                let parsed = match settings::parse(&text) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        log::warn!("remote_control: parsing on-disk file: {err:#}");
                        continue;
                    }
                };
                if this
                    .update(cx, |this, cx| {
                        if this.settings != parsed {
                            this.settings = parsed;
                            this.notify_changed(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self._watcher = Some(task);
    }
}

fn generate_secret() -> Result<String> {
    let mut buf = [0u8; CLIENT_SECRET_BYTES];
    OsRng
        .try_fill_bytes(&mut buf)
        .map_err(|err| anyhow!("OS RNG unavailable: {err}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use std::collections::HashSet;

    fn new_store(cx: &mut TestAppContext) -> Entity<RemoteControlStore> {
        cx.new(|_| RemoteControlStore::empty())
    }

    #[gpui::test]
    fn secret_length_and_distinctness(cx: &mut TestAppContext) {
        let store = new_store(cx);
        let (a, b) = store.update(cx, |store, cx| {
            let a = store.add_client("Phone".into(), cx).expect("add a");
            let b = store.add_client("Tablet".into(), cx).expect("add b");
            (a, b)
        });

        // Base64 of 32 random bytes is always 44 ASCII chars (no padding ambiguity).
        const EXPECTED_BASE64_LEN: usize = 44;
        assert_eq!(a.secret_base64.len(), EXPECTED_BASE64_LEN);
        assert_eq!(b.secret_base64.len(), EXPECTED_BASE64_LEN);

        // The encoded value must decode back to exactly 32 raw bytes.
        let raw_a = base64::engine::general_purpose::STANDARD
            .decode(&a.secret_base64)
            .expect("decode a");
        assert_eq!(raw_a.len(), CLIENT_SECRET_BYTES);
        let raw_b = base64::engine::general_purpose::STANDARD
            .decode(&b.secret_base64)
            .expect("decode b");
        assert_eq!(raw_b.len(), CLIENT_SECRET_BYTES);

        // Different invocations must not collide. (256 bits of entropy → P
        // (collision) over 2 draws is ~2⁻²⁵⁶; test is effectively deterministic.)
        assert_ne!(a.secret_base64, b.secret_base64);
    }

    #[gpui::test]
    fn add_then_remove_client(cx: &mut TestAppContext) {
        let store = new_store(cx);
        store.update(cx, |store, cx| {
            store.add_client("Phone".into(), cx).expect("added");
            assert_eq!(store.settings().clients.len(), 1);
            assert!(store.remove_client("Phone", cx));
            assert!(store.settings().clients.is_empty());
            // Removing a missing client is a no-op returning false.
            assert!(!store.remove_client("Phone", cx));
        });
    }

    #[gpui::test]
    fn add_client_rejects_duplicate(cx: &mut TestAppContext) {
        let store = new_store(cx);
        store.update(cx, |store, cx| {
            store.add_client("Phone".into(), cx).expect("first");
            let err = store
                .add_client("Phone".into(), cx)
                .expect_err("duplicate must error");
            assert!(format!("{err:#}").contains("already exists"));
        });
    }

    #[gpui::test]
    fn add_client_rejects_empty_name(cx: &mut TestAppContext) {
        let store = new_store(cx);
        store.update(cx, |store, cx| {
            assert!(store.add_client("".into(), cx).is_err());
            assert!(store.add_client("   ".into(), cx).is_err());
        });
    }

    #[gpui::test]
    fn set_enabled_emits_change_event(cx: &mut TestAppContext) {
        use std::sync::{Arc, Mutex};
        let events: Arc<Mutex<Vec<RemoteControlStoreEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let store = new_store(cx);
        let _subscription = store.update(cx, |_, cx| {
            let events = events.clone();
            cx.subscribe(&cx.entity(), move |_, _, event: &RemoteControlStoreEvent, _| {
                events.lock().expect("lock").push(event.clone());
            })
        });

        store.update(cx, |store, cx| {
            store.set_enabled(true, cx);
            assert!(store.settings().enabled);
            // Setting the same value again is a no-op (no extra event).
            store.set_enabled(true, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            events.lock().expect("lock").len(),
            1,
            "expected exactly one Changed event"
        );
    }

    #[gpui::test]
    fn set_address_normalizes_empty_to_none(cx: &mut TestAppContext) {
        let store = new_store(cx);
        store.update(cx, |store, cx| {
            store.set_address(Some("203.0.113.1".into()), cx);
            assert_eq!(
                store.settings().server_address.as_deref(),
                Some("203.0.113.1")
            );
            store.set_address(Some("".into()), cx);
            assert!(store.settings().server_address.is_none());
            store.set_address(Some("   \t".into()), cx);
            assert!(store.settings().server_address.is_none());
            store.set_address(Some("198.51.100.5 ".into()), cx);
            assert_eq!(
                store.settings().server_address.as_deref(),
                Some("198.51.100.5"),
                "trailing whitespace must be stripped"
            );
        });
    }

    #[gpui::test]
    fn set_port_updates(cx: &mut TestAppContext) {
        let store = new_store(cx);
        store.update(cx, |store, cx| {
            assert_eq!(store.settings().server_port, crate::model::DEFAULT_PORT);
            store.set_port(9090, cx);
            assert_eq!(store.settings().server_port, 9090);
        });
    }

    #[gpui::test]
    fn secrets_are_non_colliding_across_many_draws(cx: &mut TestAppContext) {
        let store = new_store(cx);
        let secrets: HashSet<String> = store.update(cx, |store, cx| {
            (0..16)
                .map(|i| {
                    store
                        .add_client(format!("Client{i}"), cx)
                        .expect("add")
                        .secret_base64
                })
                .collect()
        });
        assert_eq!(secrets.len(), 16, "16 fresh secrets should all be unique");
    }
}
