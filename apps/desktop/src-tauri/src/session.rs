//! The unlocked identity and the running client, shared by all commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cypher_client::{Client, Config, TorConfig, Unlocked};
use cypher_core::Event;
use cypher_types::PeerId;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::{Mutex, MutexGuard, broadcast};
use tokio::task::JoinHandle;

use crate::commands::transfer::release_reservation;
use crate::ephemeral::{Linking, Transfers, Voice};

pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) type CmdResult<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct Session {
    client: Client,
    events: broadcast::Sender<Event>,
    pump: JoinHandle<()>,
}

impl Session {
    /// Stops the client and waits until it has stopped.
    async fn close(self) {
        self.client.shutdown().await;
        self.pump.abort();
    }
}

/// Connection parameters of the current session, reused on restarts.
#[derive(Clone, Default)]
pub(crate) struct Endpoint {
    pub gateway_addr: String,
    /// Anonymous mode: inbox only via the onion relay, reached through Tor.
    pub anonymous: bool,
    pub bridges: Vec<String>,
}

/// Where the app keeps its data, saves accepted files and finds Lyrebird;
/// resolved once at startup.
pub(crate) struct Paths {
    pub data: PathBuf,
    /// `None` on platforms without a downloads folder.
    pub downloads: Option<PathBuf>,
    /// Lyrebird shipped next to the executable, if the build bundles it.
    pub transport: Option<PathBuf>,
}

impl Paths {
    pub(crate) fn resolve<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Self> {
        let path = app.path();
        let name = if cfg!(windows) {
            "lyrebird.exe"
        } else {
            "lyrebird"
        };
        Ok(Self {
            data: path.app_data_dir()?,
            downloads: path.download_dir().ok(),
            transport: path
                .resource_dir()
                .ok()
                .map(|dir| dir.join(name))
                .filter(|p| p.exists()),
        })
    }
}

pub(crate) struct AppState {
    paths: Paths,
    tls: Arc<rustls::ClientConfig>,
    /// Held while a client starts or stops (connect, and the stop before a
    /// lock or a new identity), so one client at a time opens the
    /// identity's data and a stop never misses a client still starting.
    /// Commands never take it: `identity` and `session` are each locked only
    /// for a moment, so they answer while a client starts.
    lifecycle: Mutex<()>,
    identity: Mutex<Option<Arc<Unlocked>>>,
    session: Mutex<Option<Session>>,
    endpoint: Mutex<Endpoint>,
    pub voice: Voice,
    pub transfers: Transfers,
    pub linking: Linking,
    /// Tests hold it to keep a client starting.
    #[cfg(test)]
    pub held_start: Mutex<()>,
}

impl AppState {
    pub(crate) fn new(paths: Paths, tls: Arc<rustls::ClientConfig>) -> Self {
        Self {
            paths,
            tls,
            lifecycle: Mutex::default(),
            identity: Mutex::default(),
            session: Mutex::default(),
            endpoint: Mutex::default(),
            voice: Voice::default(),
            transfers: Transfers::default(),
            linking: Linking::default(),
            #[cfg(test)]
            held_start: Mutex::default(),
        }
    }

    pub(crate) fn tls(&self) -> Arc<rustls::ClientConfig> {
        Arc::clone(&self.tls)
    }

    pub(crate) fn paths(&self) -> &Paths {
        &self.paths
    }

    pub(crate) async fn set_identity(&self, identity: Unlocked) {
        let _lifecycle = self.stop().await;
        *self.identity.lock().await = Some(Arc::new(identity));
    }

    /// The safety number with `peer`; needs only the unlocked identity.
    pub(crate) async fn safety_number(&self, peer: &PeerId) -> CmdResult<String> {
        let identity = self.identity.lock().await;
        let own = identity
            .as_ref()
            .ok_or("identity is locked")?
            .seed
            .derive_identity()
            .peer_id();
        Ok(cypher_core::ui::safety_number(&own, peer))
    }

    pub(crate) async fn endpoint(&self) -> Endpoint {
        self.endpoint.lock().await.clone()
    }

    /// (Re)starts the client for the unlocked identity; every event is also
    /// handed to `emit` (the webview in the app).
    pub(crate) async fn connect(
        &self,
        endpoint: Endpoint,
        emit: impl Fn(&Event) + Send + 'static,
    ) -> CmdResult<String> {
        // One client at a time: the old one is gone before the new one opens
        // the same data, and a second `connect` waits for this one. Meanwhile
        // commands find no client and say so.
        let _lifecycle = self.lifecycle.lock().await;
        let old = self.session.lock().await.take();
        if let Some(old) = old {
            old.close().await;
        }
        let identity = self.identity.lock().await.clone();
        let identity = identity.ok_or("identity is locked")?;
        let config = Config {
            gateway_addr: endpoint.gateway_addr.clone(),
            tls: Arc::clone(&self.tls),
            data_dir: self.paths.data.clone(),
            require_onion: endpoint.anonymous,
            tor: endpoint.anonymous.then(|| TorConfig {
                bridges: endpoint.bridges.clone(),
                transport_binary: self.paths.transport.clone(),
            }),
        };
        *self.endpoint.lock().await = endpoint;
        #[cfg(test)]
        drop(self.held_start.lock().await);
        let (client, mut rx) = Client::start(&identity.seed, identity.device, config)
            .await
            .map_err(err)?;
        // Before anything else is sent: contacts greeted later hear it too.
        client
            .set_profile_name(Some(identity.nickname.clone()))
            .await
            .map_err(err)?;
        let (events, _) = broadcast::channel(256);
        let (tx, offers) = (events.clone(), self.transfers.offers.clone());
        let pump = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Some(reserved) = offers.track(&event) {
                    release_later(reserved);
                }
                emit(&event);
                let _ = tx.send(event);
            }
        });
        let me = client.peer_id().to_hex();
        *self.session.lock().await = Some(Session {
            client,
            events,
            pump,
        });
        Ok(me)
    }

    pub(crate) async fn client(&self) -> CmdResult<Client> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|s| s.client.clone())
            .ok_or_else(|| "not connected".to_owned())
    }

    /// Client plus an event subscription taken before any command is sent,
    /// so replies cannot be missed.
    pub(crate) async fn client_and_events(
        &self,
    ) -> CmdResult<(Client, broadcast::Receiver<Event>)> {
        let guard = self.session.lock().await;
        let s = guard.as_ref().ok_or("not connected")?;
        Ok((s.client.clone(), s.events.subscribe()))
    }

    /// Stops the client and forgets the unlocked identity.
    pub(crate) async fn lock(&self) {
        let _lifecycle = self.stop().await;
        *self.identity.lock().await = None;
    }

    /// Closes the microphone of a voice note being recorded (first, before
    /// waiting on a client still starting), stops the client, a link being
    /// waited for, and forgets offered and dropped files. The guard returned
    /// keeps a new client from starting until the caller has changed the
    /// identity.
    async fn stop(&self) -> MutexGuard<'_, ()> {
        self.voice.reset().await;
        let lifecycle = self.lifecycle.lock().await;
        // Again, for a microphone opened while a client was starting.
        self.voice.reset().await;
        let session = self.session.lock().await.take();
        if let Some(s) = session {
            s.close().await;
        }
        self.linking.cancel().await;
        self.transfers.reset();
        lifecycle
    }
}

/// Releases the name reserved for a file whose transfer failed, off the
/// event pump.
fn release_later(reserved: PathBuf) {
    tokio::spawn(async move { release_reservation(&reserved).await });
}

/// Waits for the first event `pick` accepts.
pub(crate) async fn await_event<T>(
    rx: &mut broadcast::Receiver<Event>,
    pick: impl FnMut(&Event) -> Option<CmdResult<T>>,
) -> CmdResult<T> {
    await_event_within(REPLY_TIMEOUT, rx, pick).await
}

/// [`await_event`], giving up after `wait`. Events skipped because the
/// receiver fell behind may have held the reply: waiting goes on, since the
/// reply may still come, but a wait that then runs out says it missed
/// events (`lagged`) rather than that no reply came (`timed out`).
pub(crate) async fn await_event_within<T>(
    wait: Duration,
    rx: &mut broadcast::Receiver<Event>,
    mut pick: impl FnMut(&Event) -> Option<CmdResult<T>>,
) -> CmdResult<T> {
    let mut lagged = false;
    let waited = tokio::time::timeout(wait, async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(result) = pick(&event) {
                        return result;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => lagged = true,
                Err(broadcast::error::RecvError::Closed) => return Err("client stopped".to_owned()),
            }
        }
    })
    .await;
    waited.unwrap_or_else(|_| {
        let reason = if lagged { "lagged" } else { "timed out" };
        Err(reason.to_owned())
    })
}

/// System trust roots, unless development certificates are pinned: by the
/// PEM file `CYPHER_DEV_CA` names at startup or, in a debug build, by the PEM
/// text `CYPHER_DEV_CA_PEM` held at compile time (for devices whose
/// environment the developer cannot set, such as an Android emulator).
pub(crate) fn tls_from_env() -> CmdResult<Arc<rustls::ClientConfig>> {
    if let Some(path) = std::env::var_os("CYPHER_DEV_CA") {
        return tls(Some(&PathBuf::from(path)));
    }
    #[cfg(debug_assertions)]
    if let Some(pem) = option_env!("CYPHER_DEV_CA_PEM") {
        return cypher_tls::make_client_config_with_pem(pem).map_err(err);
    }
    tls(None)
}

/// Trusts only the certificates in the `dev_ca` PEM file when given, the
/// system roots otherwise.
pub(crate) fn tls(dev_ca: Option<&Path>) -> CmdResult<Arc<rustls::ClientConfig>> {
    let Some(path) = dev_ca else {
        return Ok(cypher_tls::make_client_config());
    };
    let pem = std::fs::read_to_string(path).map_err(err)?;
    cypher_tls::make_client_config_with_pem(&pem).map_err(err)
}
