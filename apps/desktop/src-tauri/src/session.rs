//! The unlocked identity and the running client, shared by all commands.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use cypher_client::{Client, Config, TorConfig};
use cypher_core::Event;
use cypher_crypto::IdentitySeed;
use cypher_media::Recorder;
use cypher_types::FileId;
use tauri::{AppHandle, Manager};
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;

use crate::dto;

pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) type CmdResult<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct Identity {
    seed: IdentitySeed,
    nickname: String,
}

struct Session {
    client: Client,
    events: broadcast::Sender<Event>,
    pump: JoinHandle<()>,
}

/// Names of files offered to us, so accepted files can be saved safely.
pub(crate) type Offers = Arc<StdMutex<HashMap<FileId, String>>>;

/// Connection parameters of the current session, reused on restarts.
#[derive(Clone, Default)]
pub(crate) struct Endpoint {
    pub gateway_addr: String,
    /// Anonymous mode: inbox only via the onion relay, reached through Tor.
    pub anonymous: bool,
    pub bridges: Vec<String>,
}

#[derive(Default)]
pub(crate) struct AppState {
    identity: Mutex<Option<Identity>>,
    session: Mutex<Option<Session>>,
    endpoint: Mutex<Endpoint>,
    pub offers: Offers,
    /// The voice note being recorded, if any.
    pub voice: StdMutex<Option<Recorder>>,
}

impl AppState {
    pub(crate) async fn set_identity(&self, seed: IdentitySeed, nickname: String) {
        self.stop().await;
        *self.identity.lock().await = Some(Identity { seed, nickname });
    }

    pub(crate) async fn nickname(&self) -> Option<String> {
        self.identity
            .lock()
            .await
            .as_ref()
            .map(|i| i.nickname.clone())
    }

    pub(crate) async fn endpoint(&self) -> Endpoint {
        self.endpoint.lock().await.clone()
    }

    /// (Re)starts the client for the unlocked identity.
    pub(crate) async fn connect(&self, app: &AppHandle, endpoint: Endpoint) -> CmdResult<String> {
        self.stop().await;
        let identity = self.identity.lock().await;
        let identity = identity.as_ref().ok_or("identity is locked")?;
        let config = Config {
            gateway_addr: endpoint.gateway_addr.clone(),
            tls: tls_config()?,
            data_dir: data_dir(app)?,
            require_onion: endpoint.anonymous,
            tor: endpoint.anonymous.then(|| TorConfig {
                bridges: endpoint.bridges.clone(),
                transport_binary: bundled_transport(app),
            }),
        };
        *self.endpoint.lock().await = endpoint;
        let (client, mut rx) = Client::start(&identity.seed, config).await.map_err(err)?;
        let (events, _) = broadcast::channel(256);
        let (tx, app, offers) = (events.clone(), app.clone(), Arc::clone(&self.offers));
        let pump = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Event::TransferOffered { file_id, name, .. } = &event
                    && let Ok(mut offers) = offers.lock()
                {
                    offers.insert(*file_id, name.clone());
                }
                dto::emit(&app, &event);
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

    pub(crate) async fn stop(&self) {
        let session = self.session.lock().await.take();
        if let Some(s) = session {
            s.client.shutdown().await;
            s.pump.abort();
        }
    }
}

/// Waits for the first event `pick` accepts.
pub(crate) async fn await_event<T>(
    rx: &mut broadcast::Receiver<Event>,
    mut pick: impl FnMut(&Event) -> Option<CmdResult<T>>,
) -> CmdResult<T> {
    tokio::time::timeout(REPLY_TIMEOUT, async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(result) = pick(&event) {
                        return result;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Err("client stopped".to_owned()),
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err("timed out".to_owned()))
}

pub(crate) fn data_dir(app: &AppHandle) -> CmdResult<PathBuf> {
    app.path().app_data_dir().map_err(err)
}

/// Lyrebird shipped next to the executable, if the build bundles it.
fn bundled_transport(app: &AppHandle) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "lyrebird.exe"
    } else {
        "lyrebird"
    };
    app.path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join(name))
        .filter(|p| p.exists())
}

/// System trust roots; `CYPHER_DEV_CA` pins development certificates instead.
fn tls_config() -> CmdResult<Arc<rustls::ClientConfig>> {
    match std::env::var("CYPHER_DEV_CA") {
        Ok(path) => {
            let pem = std::fs::read_to_string(path).map_err(err)?;
            cypher_tls::make_client_config_with_pem(&pem).map_err(err)
        }
        Err(_) => Ok(cypher_tls::make_client_config()),
    }
}
