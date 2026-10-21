//! Commands driven through Tauri's mock runtime by name, with the same
//! camelCase arguments the webview sends (`apps/desktop/src/tauri.ts`), so a
//! renamed parameter or a command missing from the handler list fails here.
//! The two-desktop scenario needs `CYPHER_TEST_REDIS` and `CYPHER_TEST_NATS`
//! (see `tests/e2e`): skipped without them, failed where
//! `CYPHER_REQUIRE_LIVE=1`.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use cypher_core::Event;
use cypher_media::Recorder;
use cypher_types::FileId;
use serde_json::{Value, json};
use tauri::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};
use tauri::webview::InvokeRequest;
use tauri::{App, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tempfile::TempDir;

use crate::commands::names::COMMANDS;
use crate::ephemeral::NotKept;
use crate::session::{self, AppState, Paths};
use crate::{media_scheme, shell};

const PASS: &str = "correct horse battery";
const WAIT: Duration = Duration::from_secs(20);
/// The sample rate of the stand-in microphone.
const RATE: u32 = 48_000;

/// One desktop app: mock runtime and webview, its own data and downloads
/// folders.
struct Desktop {
    app: App<MockRuntime>,
    webview: WebviewWindow<MockRuntime>,
    dir: TempDir,
}

impl Desktop {
    fn new(tls: Arc<rustls::ClientConfig>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().join("data"),
            downloads: Some(dir.path().join("downloads")),
            transport: None,
        };
        std::fs::create_dir_all(dir.path().join("downloads")).unwrap();
        let app = crate::wire(mock_builder()).build(app_context()).unwrap();
        app.manage(AppState::new(paths, tls));
        app.manage(shell::CloseToTray::default());
        let webview = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
            .build()
            .unwrap();
        Self { app, webview, dir }
    }

    /// Calls `cmd` through the IPC bridge, as the webview's `invoke` does.
    async fn invoke(
        &self,
        cmd: &str,
        body: InvokeBody,
        headers: HeaderMap,
    ) -> Result<Value, Value> {
        invoke_from(&self.webview, LOCAL_ORIGIN, cmd, body, headers).await
    }

    /// `invoke(cmd, args)` from the webview; an error is the command's message.
    async fn call(&self, cmd: &str, args: Value) -> Result<Value, String> {
        self.invoke(cmd, InvokeBody::Json(args), HeaderMap::new())
            .await
            .map_err(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned))
    }

    fn state(&self) -> State<'_, AppState> {
        self.app.state()
    }

    async fn create(&self, nickname: &str) -> String {
        let args = json!({ "nickname": nickname, "passphrase": PASS });
        text(&self.call("create_identity", args).await.unwrap())
    }

    async fn connect(&self, gateway: &str) -> String {
        let args = json!({ "addr": gateway, "anonymous": false, "bridges": [] });
        text(&self.call("connect_to_gateway", args).await.unwrap())
    }
}

/// The app's own origin, which differs per platform.
const LOCAL_ORIGIN: &str = if cfg!(windows) {
    "http://tauri.localhost"
} else {
    "tauri://localhost"
};

/// The mock runtime's context with the app's own access control: the
/// capabilities and command permissions `build.rs` generates.
fn app_context() -> tauri::Context<MockRuntime> {
    let mut context = mock_context(noop_assets());
    let mut app = crate::context::<MockRuntime>();
    std::mem::swap(context.runtime_authority_mut(), app.runtime_authority_mut());
    context
}

/// Calls `cmd` from `webview` showing a page of `origin`.
async fn invoke_from(
    webview: &WebviewWindow<MockRuntime>,
    origin: &str,
    cmd: &str,
    body: InvokeBody,
    headers: HeaderMap,
) -> Result<Value, Value> {
    let request = InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: origin.parse().unwrap(),
        body,
        headers,
        invoke_key: INVOKE_KEY.into(),
    };
    let webview = webview.clone();
    tokio::task::spawn_blocking(move || get_ipc_response(&webview, request))
        .await
        .unwrap()
        .map(|body| body.deserialize().unwrap())
}

fn text(value: &Value) -> String {
    value.as_str().expect("a string").to_owned()
}

/// A recorder that hears `samples` of a square wave instead of a microphone.
fn recorder(samples: usize) -> Recorder {
    let wave: Vec<f32> = (0..samples)
        .map(|i| if i % 96 < 48 { 0.3 } else { -0.3 })
        .collect();
    Recorder::from_samples(RATE, &wave).unwrap()
}

/// Polls `probe` until it yields a value or the wait runs out.
async fn eventually<T, F: Future<Output = Option<T>>>(mut probe: impl FnMut() -> F) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(v) = probe().await {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("condition met in time")
}

/// `connect` returns before the session is up (the UI waits for
/// `cypher://connected`), so commands that need the server are retried while
/// the client reports being offline; any other outcome is returned.
async fn once_online<T, F: Future<Output = Result<T, String>>>(
    mut command: impl FnMut() -> F,
) -> Result<T, String> {
    tokio::time::timeout(WAIT, async {
        loop {
            match command().await {
                Err(e) if e == "Offline" => tokio::time::sleep(Duration::from_millis(50)).await,
                other => return other,
            }
        }
    })
    .await
    .expect("client online in time")
}

#[tokio::test]
async fn identity_lifecycle_without_a_server() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let no_args = || json!({});
    assert_eq!(
        desktop.call("has_identity", no_args()).await,
        Ok(json!(false))
    );
    assert!(is_locked(&desktop).await);

    let peer = desktop.create("alice").await;
    assert_eq!(
        desktop.call("has_identity", no_args()).await,
        Ok(json!(true))
    );
    let bob = json!({ "peerId": cypher_types::PeerId([9; 32]).to_hex() });
    let number = text(&desktop.call("safety_number", bob).await.unwrap());
    assert!(number.len() == 60 && number.bytes().all(|d| d.is_ascii_digit()));
    let invalid = desktop
        .call("safety_number", json!({ "peerId": "zz" }))
        .await;
    assert_eq!(invalid.unwrap_err(), "invalid peer id");
    assert!(!is_locked(&desktop).await);

    let with_pass = |passphrase: &str| json!({ "passphrase": passphrase });
    desktop
        .call("unlock_identity", with_pass("wrong"))
        .await
        .unwrap_err();
    let unlocked = desktop.call("unlock_identity", with_pass(PASS)).await;
    assert_eq!(unlocked, Ok(json!([peer, "alice"])));

    desktop
        .call("export_mnemonic", with_pass("wrong"))
        .await
        .unwrap_err();
    let mnemonic = desktop
        .call("export_mnemonic", with_pass(PASS))
        .await
        .unwrap();
    let restored = Desktop::new(cypher_tls::make_client_config());
    let args = json!({ "mnemonic": mnemonic, "nickname": "alice", "passphrase": PASS });
    assert_eq!(
        restored.call("import_mnemonic", args).await,
        Ok(json!(peer)),
        "the recovery phrase restores the same identity"
    );

    lock_then_erase(&desktop, &peer, &text(&mnemonic)).await;
}

/// Whether the identity is forgotten: a safety number needs it unlocked.
async fn is_locked(desktop: &Desktop) -> bool {
    let bob = json!({ "peerId": cypher_types::PeerId([9; 32]).to_hex() });
    desktop.call("safety_number", bob).await == Err("identity is locked".to_owned())
}

/// Locking forgets the identity until the passphrase; erasing, for a
/// forgotten one, makes room to start over.
async fn lock_then_erase(desktop: &Desktop, peer: &str, mnemonic: &str) {
    let no_args = || json!({});
    let with_pass = |passphrase: &str| json!({ "passphrase": passphrase });
    // Locking forgets the identity until the passphrase is typed again.
    desktop.call("lock", no_args()).await.unwrap();
    assert!(is_locked(desktop).await);
    assert_eq!(
        desktop.call("export_mnemonic", with_pass(PASS)).await,
        Ok(json!(mnemonic))
    );
    let unlocked = desktop.call("unlock_identity", with_pass(PASS)).await;
    assert_eq!(unlocked, Ok(json!([peer, "alice"])));

    // A forgotten passphrase: erase, then start over on the same device.
    desktop.call("erase_device", no_args()).await.unwrap();
    assert_eq!(
        desktop.call("has_identity", no_args()).await,
        Ok(json!(false))
    );
    assert!(is_locked(desktop).await);
    let args = json!({ "mnemonic": mnemonic, "nickname": "alice", "passphrase": PASS });
    assert_eq!(desktop.call("import_mnemonic", args).await, Ok(json!(peer)));
}

#[tokio::test]
async fn commands_fail_cleanly_before_connecting() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let call = |cmd: &'static str, args: Value| desktop.call(cmd, args);
    let peer = cypher_types::PeerId([1; 32]).to_hex();
    let connect = |addr: &str| json!({ "addr": addr, "anonymous": false, "bridges": [] });

    let locked = call("connect_to_gateway", connect("localhost:1")).await;
    assert_eq!(locked.unwrap_err(), "identity is locked");
    let no_identity = call("safety_number", json!({ "peerId": peer })).await;
    assert_eq!(no_identity.unwrap_err(), "identity is locked");
    desktop.create("bob").await;
    call("connect_to_gateway", connect("no port"))
        .await
        .unwrap_err();

    let not_connected = Err("not connected".to_owned());
    assert_eq!(call("get_conversations", json!({})).await, not_connected);
    // Push: desktops have no distributor, so no subscription and no client.
    assert_eq!(call("push_enable", json!({})).await, not_connected);
    assert_eq!(call("push_disable", json!({})).await, not_connected);
    let key = |k: String| json!({ "key": k });
    let subscribed = call("push_subscribe", key(format!("04{}", "ab".repeat(64)))).await;
    assert_eq!(subscribed, Ok(Value::Bool(false)));
    let bad = call("push_subscribe", key("04".into())).await;
    assert_eq!(bad.unwrap_err(), "invalid push key");
    assert_eq!(call("reconnect", json!({})).await, not_connected);
    let send = |peer_id: &str| json!({ "peerId": peer_id, "text": "x" });
    assert_eq!(call("send_message", send(&peer)).await, not_connected);
    assert_eq!(
        call("send_message", send("zz")).await.unwrap_err(),
        "invalid peer id"
    );
    let snake_case = call("send_message", json!({ "peer_id": peer, "text": "x" }));
    assert!(
        snake_case.await.unwrap_err().contains("peerId"),
        "the webview sends camelCase"
    );

    let history = json!({ "peerId": "zz", "limit": 10, "before": null });
    call("get_history", history).await.unwrap_err();
    call("clear_chat_history", json!({})).await.unwrap_err();
    call("create_link", json!({})).await.unwrap_err();
    let file = |id: String| json!({ "fileId": id });
    call("accept_file", file(FileId([2; 16]).to_hex()))
        .await
        .unwrap_err();
    call("cancel_transfer", file("zz".into()))
        .await
        .unwrap_err();
    // The peer is checked before the native file dialog opens.
    let browse = call("browse_and_send", json!({ "peerId": "zz" }));
    assert_eq!(browse.await.unwrap_err(), "invalid peer id");
    let stop = call("voice_stop", json!({ "peerId": peer })).await;
    assert_eq!(stop.unwrap_err(), "not recording");
    assert_eq!(call("voice_cancel", json!({})).await, Ok(Value::Null));

    desktop.state().voice.hold(recorder(4_800));
    let again = call("voice_start", json!({})).await;
    assert_eq!(again.unwrap_err(), "already recording");
    assert_eq!(call("voice_cancel", json!({})).await, Ok(Value::Null));
    assert!(!desktop.state().voice.is_recording());

    let note = media_request(&FileId([3; 16]).to_hex(), None);
    let offline = media_scheme::answer(desktop.app.handle(), &note).await;
    assert_eq!(offline.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// A malformed id is refused in the words the web client uses for it
/// (`cypher-wasm`'s command tests), before the connection is looked at.
#[tokio::test]
async fn bad_ids_are_named_as_the_web_client_names_them() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    desktop.create("bob").await;
    let refused = async |cmd: &str, args: Value| desktop.call(cmd, args).await.unwrap_err();
    let peer = cypher_types::PeerId([1; 32]).to_hex();

    let bad_peer = json!({ "peerId": "zz" });
    assert_eq!(refused("accept_contact", bad_peer).await, "invalid peer id");
    let message = "invalid message id";
    let reply = json!({ "peerId": peer, "text": "x", "replyTo": "zz" });
    assert_eq!(refused("send_message", reply).await, message);
    let delete = json!({ "peerId": peer, "msgId": "zz", "timestamp": 0 });
    assert_eq!(refused("delete_message", delete).await, message);
    let file = "invalid file id";
    let bad_file = || json!({ "fileId": "zz" });
    assert_eq!(refused("accept_file", bad_file()).await, file);
    assert_eq!(refused("cancel_transfer", bad_file()).await, file);
    // A bad id in a read receipt list is skipped, not refused (the web
    // client refuses the list).
    let ids = json!({ "peerId": peer, "msgIds": ["zz"] });
    assert_ne!(refused("mark_read", ids).await, message);
}

/// Locking or erasing closes the microphone and gives up a link being
/// waited for, so nothing recorded meanwhile reaches the next identity.
#[tokio::test]
async fn lock_and_erase_stop_the_microphone() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    desktop.create("alice").await;
    let state = desktop.state();
    for cmd in ["lock", "erase_device"] {
        state.voice.hold(recorder(4_800));
        let start = json!({ "addr": "127.0.0.1:1", "name": "Laptop" });
        desktop.call("link_start", start).await.unwrap();

        desktop.call(cmd, json!({})).await.unwrap();
        assert!(!state.voice.is_recording(), "still recording after {cmd}");
        assert!(state.linking.take().await.is_none(), "linking after {cmd}");
    }

    // A microphone that opens while the app locks is closed again.
    let ticket = state.voice.ticket().unwrap();
    desktop.call("lock", json!({})).await.unwrap();
    let late = state.voice.keep(ticket, recorder(4_800));
    assert!(matches!(late, Err(NotKept::Reset(_))));
    assert!(!state.voice.is_recording());
}

fn offer_of(file_id: FileId) -> Event {
    Event::TransferOffered {
        peer: cypher_types::PeerId([1; 32]),
        file_id,
        name: "notes.txt".into(),
        size: 5,
        mime: "text/plain".into(),
    }
}

/// Locking or erasing forgets files offered to the identity and the last
/// drop; an accept under way when it happens puts nothing back.
#[tokio::test]
async fn lock_and_erase_forget_offers_and_drops() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    desktop.create("alice").await;
    let state = desktop.state();
    let offers = &state.transfers.offers;
    let file = FileId([5; 16]);
    for cmd in ["lock", "erase_device"] {
        offers.track(&offer_of(file));
        let dropped = vec![desktop.dir.path().to_owned()];
        crate::commands::transfer::files_dropped(desktop.app.handle(), dropped);
        let drop_id = state.transfers.dropped.lock().unwrap().id_for_tests();

        desktop.call(cmd, json!({})).await.unwrap();
        assert!(!offers.contains(&file), "offer kept after {cmd}");
        let peer = cypher_types::PeerId([1; 32]).to_hex();
        let send = json!({ "peerId": peer, "id": drop_id });
        let gone = desktop.call("send_dropped", send).await;
        assert_eq!(gone.unwrap_err(), "this drop is gone", "after {cmd}");
    }

    offers.track(&offer_of(file));
    let (name, ticket) = offers.take(&file).unwrap();
    desktop.call("lock", json!({})).await.unwrap();
    offers.restore(file, name, ticket);
    assert!(
        !offers.contains(&file),
        "an earlier identity's offer came back"
    );
}

/// An offer stays open when accepting it fails, and goes when its transfer
/// fails or completes; an accepted file whose transfer fails hands back
/// its reserved name to release.
#[tokio::test]
async fn an_offer_lasts_until_accepted_or_ended() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    desktop.create("bob").await;
    let offers = &desktop.state().transfers.offers;
    let file = FileId([6; 16]);
    offers.track(&offer_of(file));
    let accept = desktop.call("accept_file", json!({ "fileId": file.to_hex() }));
    assert_eq!(accept.await.unwrap_err(), "not connected");
    assert!(offers.contains(&file), "a failed accept keeps the offer");
    let downloads = std::fs::read_dir(desktop.dir.path().join("downloads")).unwrap();
    assert_eq!(downloads.count(), 0, "no name reserved");

    let failed = Event::TransferFailed {
        file_id: file,
        reason: cypher_core::FailReason::Cancelled,
    };
    assert_eq!(offers.track(&failed), None);
    assert!(!offers.contains(&file), "a failed transfer's offer goes");
    offers.track(&offer_of(file));
    offers.track(&Event::TransferComplete { file_id: file });
    assert!(!offers.contains(&file), "a completed transfer's offer goes");

    offers.track(&offer_of(file));
    let (_, ticket) = offers.take(&file).unwrap();
    let reserved = desktop.dir.path().join("downloads").join("notes.txt");
    offers.accepted(file, reserved.clone(), ticket);
    assert_eq!(offers.track(&failed), Some(reserved));
    assert_eq!(offers.track(&failed), None, "released once");
}

/// A cancel that comes after `link_finish` took the wait, but before it
/// started waiting, is not lost; a new wait starts uncancelled.
#[tokio::test]
async fn a_link_cancel_is_kept_until_the_wait_begins() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let start = || json!({ "addr": "127.0.0.1:1", "name": "Laptop" });
    desktop.call("link_start", start()).await.unwrap();
    let (waiting, cancel) = desktop.state().linking.take().await.unwrap();
    desktop.call("link_cancel", json!({})).await.unwrap();
    tokio::time::timeout(WAIT, cancel.cancelled())
        .await
        .expect("the cancel waited for the finish");
    drop(waiting);

    desktop.call("link_start", start()).await.unwrap();
    let (_waiting, fresh) = desktop.state().linking.take().await.unwrap();
    assert!(!fresh.is_cancelled());
    assert!(desktop.state().linking.take().await.is_none(), "taken once");
}

/// The command names `tauri.ts` passes to `invoke(…)` or its `command(…)`
/// wrapper.
fn names_the_ui_invokes(source: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("(\"") {
        let (before, after) = rest.split_at(at);
        rest = after.get(2..).unwrap_or_default();
        // `invoke<T>("…")`: the type argument sits between name and call.
        let callee = match before.strip_suffix('>') {
            Some(generic) => generic.rsplit_once('<').map_or("", |(name, _)| name),
            None => before,
        };
        let callee = callee
            .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next()
            .unwrap_or_default();
        if matches!(callee, "invoke" | "command") {
            names.insert(rest.split('"').next().unwrap_or_default());
        }
    }
    names
}

/// The UI calls exactly the listed commands, no more, no fewer.
#[test]
fn the_ui_invokes_exactly_the_listed_commands() {
    let ui = include_str!("../../src/tauri.ts");
    let listed: BTreeSet<&str> = COMMANDS.iter().copied().collect();
    assert_eq!(listed.len(), COMMANDS.len(), "a command is listed twice");
    assert_eq!(names_the_ui_invokes(ui), listed);
}

/// Registered for the tests only (see `wire`) and not listed in
/// `COMMANDS`, so no permission names it.
#[tauri::command]
pub(crate) fn unlisted_command() {}

/// Every listed command reaches its handler from the main window; a
/// registered command outside the list is refused before its handler.
#[tokio::test]
async fn every_listed_command_is_registered_and_granted() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    for &cmd in COMMANDS {
        // Holding a recording keeps `voice_start` off the real microphone.
        desktop.state().voice.hold(recorder(480));
        let Err(error) = desktop.call(cmd, json!({})).await else {
            continue;
        };
        assert_ne!(error, format!("Command {cmd} not found"));
        assert!(!error.contains("not allowed"), "{cmd}: {error}");
    }
    assert!(!COMMANDS.contains(&"unlisted_command"));
    let unlisted = desktop.call("unlisted_command", json!({})).await;
    let refused = unlisted.expect_err("an unlisted command ran");
    assert!(refused.contains("not allowed"), "{refused}");
}

/// App commands answer only the main window showing the app's own
/// pages: another webview, or a remote page in the main window, is refused.
#[tokio::test]
async fn other_webviews_and_remote_pages_cannot_call_the_app() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    desktop.create("alice").await;
    let other = WebviewWindowBuilder::new(&desktop.app, "other", WebviewUrl::default())
        .build()
        .unwrap();
    let pass = || InvokeBody::Json(json!({ "passphrase": PASS }));
    let from_other = invoke_from(
        &other,
        LOCAL_ORIGIN,
        "export_mnemonic",
        pass(),
        HeaderMap::new(),
    )
    .await;
    let refused = from_other.expect_err("an unlisted webview read the phrase");
    assert!(refused.to_string().contains("not allowed"), "{refused}");

    let remote = invoke_from(
        &desktop.webview,
        "https://evil.example",
        "export_mnemonic",
        pass(),
        HeaderMap::new(),
    )
    .await;
    let refused = remote.expect_err("a remote page read the phrase");
    assert!(refused.to_string().contains("not allowed"), "{refused}");

    let main = desktop.call("export_mnemonic", json!({ "passphrase": PASS }));
    assert!(main.await.is_ok(), "the app's own page may");
}

#[tokio::test]
async fn qr_codes_are_png_data_uris() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let uri = desktop
        .call("generate_qr", json!({ "linkId": "cypher-link" }))
        .await;
    assert!(text(&uri.unwrap()).starts_with("data:image/png;base64,"));
}

#[tokio::test]
async fn the_ui_words_the_tray_and_chooses_what_closing_does() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let app = desktop.app.handle();
    assert!(shell::closes_to_tray(app), "on by default");
    let args = json!({ "enabled": false });
    desktop.call("set_close_to_tray", args).await.unwrap();
    assert!(!shell::closes_to_tray(app));

    // Open in the tray menu brings the hidden window back; strays do nothing.
    desktop.webview.hide().unwrap();
    shell::on_menu(app, "open");
    shell::on_menu(app, "elsewhere");
    assert!(desktop.webview.is_visible().unwrap());

    // The mock runtime has no tray; the words wait for one without failing.
    let args = json!({ "open": "Open", "quit": "Quit", "tooltip": "Cypher · 2 unread" });
    desktop.call("set_tray", args).await.unwrap();
}

#[test]
fn a_development_ca_replaces_the_system_roots() {
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("dev.pem");
    let cert = cypher_tls::SelfSignedCert::generate(&["localhost"]).unwrap();
    std::fs::write(&ca, cert.cert_pem).unwrap();
    session::tls(Some(&ca)).unwrap();
    session::tls(None).unwrap();

    session::tls(Some(&dir.path().join("missing.pem"))).unwrap_err();
    std::fs::write(&ca, "not a certificate").unwrap();
    session::tls(Some(&ca)).unwrap_err();
}

#[test]
fn paths_come_from_the_platform() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let paths = Paths::resolve(desktop.app.handle()).unwrap();
    assert!(paths.data.ends_with(&desktop.app.config().identifier));
    assert!(paths.transport.is_none(), "tests bundle no Lyrebird");
}

fn media_request(file_id: &str, range: Option<&str>) -> Request<Vec<u8>> {
    let mut builder = Request::builder().uri(format!("cypher-media://localhost/{file_id}"));
    if let Some(range) = range {
        builder = builder.header(header::RANGE, range);
    }
    builder.body(Vec::new()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_desktops_against_in_process_stack() {
    let Some(stack) = e2e::Stack::from_env().await else {
        eprintln!("CYPHER_TEST_REDIS / CYPHER_TEST_NATS not set; skipping");
        return;
    };
    let gateway = stack.target().gateway_addr.clone();
    let (a, b) = (
        Desktop::new(Arc::clone(&stack.target().tls)),
        Desktop::new(Arc::clone(&stack.target().tls)),
    );
    a.create("alice").await;
    b.create("bob").await;
    let (a_id, b_id) = (a.connect(&gateway).await, b.connect(&gateway).await);

    pair(&a, &b, &b_id).await;
    chat(&a, &b, &a_id, &b_id).await;
    send_a_voice_note(&a, &b, &a_id, &b_id).await;
    play_a_video_note(&a, &b, &b_id).await;
    accept_an_offered_file(&a, &b, &b_id).await;
    accept_two_files_of_one_name(&a, &b, &b_id).await;
    refuse_to_open_a_program(&a, &b, &b_id).await;
    send_picked_files(&a, &b_id).await;
    send_and_preview_pictures(&a, &b, &b_id).await;
    name_then_forget_a_contact(&b, &a_id).await;
    link_a_new_desktop(&stack.target().tls, &a, &a_id, &gateway).await;

    let anonymity = json!({ "anonymous": false, "bridges": [" "] });
    a.call("apply_anonymous_settings", anonymity).await.unwrap();
    a.call("clear_chat_history", json!({})).await.unwrap();
    stack.stop().await;
}

/// A new desktop waits to be linked and the first links it: it becomes a
/// device of the same identity. Waiting can be given up.
async fn link_a_new_desktop(
    tls: &Arc<rustls::ClientConfig>,
    a: &Desktop,
    a_id: &str,
    gateway: &str,
) {
    let laptop = Desktop::new(Arc::clone(tls));
    let name = laptop.call("device_name", json!({})).await.unwrap();
    assert!(!text(&name).is_empty());
    let start = json!({ "addr": gateway, "name": "Laptop" });
    let offer = text(&laptop.call("link_start", start).await.unwrap());
    let finish = laptop.call("link_finish", json!({ "passphrase": PASS }));
    // The new desktop's throwaway session may still be signing in.
    let link = eventually(|| {
        let args = json!({ "offer": offer });
        async move { a.call("link_device", args).await.ok() }
    });
    let (finished, _) = tokio::join!(finish, link);
    let (peer, nickname): (String, String) = serde_json::from_value(finished.unwrap()).unwrap();
    assert_eq!((peer.as_str(), nickname.as_str()), (a_id, "alice"));
    a.call("unlink_device", json!({ "device": 0 }))
        .await
        .unwrap();
    a.call("replace_device", json!({ "device": 0 }))
        .await
        .unwrap();
    a.call("cancel_history", json!({})).await.unwrap();
    a.call("trust_server_key", json!({})).await.unwrap();

    let other = Desktop::new(Arc::clone(tls));
    other
        .call("link_start", json!({ "addr": gateway, "name": "Tablet" }))
        .await
        .unwrap();
    other.call("link_cancel", json!({})).await.unwrap();
    let refused = other.call("link_finish", json!({ "passphrase": PASS }));
    assert_eq!(refused.await.unwrap_err(), "not waiting to be linked");
}

/// A contact's name shows in the chat list; deleting the chat empties its
/// history and drops it from the list.
async fn name_then_forget_a_contact(b: &Desktop, a_id: &str) {
    // The name the contact created their profile with came along.
    let list = b.call("get_conversations", json!({})).await.unwrap();
    assert_eq!(list[0]["name"], "alice");
    assert_eq!(list[0]["alias"], Value::Null);
    let rename = json!({ "peerId": a_id, "alias": " Alice " });
    b.call("rename_peer", rename).await.unwrap();
    // The rename is queued to the client; the list reads what is stored.
    let list = eventually(|| async {
        let list = b.call("get_conversations", json!({})).await.ok()?;
        (list[0]["alias"] == "Alice").then_some(list)
    })
    .await;
    assert_eq!(list[0]["peer_id"], a_id);
    assert!(list[0]["last"].is_object());
    assert_eq!(list[0]["unread"].as_u64().map(|n| n > 0), Some(true));

    let invalid = b.call("rename_peer", json!({ "peerId": "zz", "alias": "x" }));
    assert_eq!(invalid.await.unwrap_err(), "invalid peer id");
    block_then_unblock(b, a_id).await;
    b.call("push_enable", json!({})).await.unwrap();
    b.call("push_disable", json!({})).await.unwrap();

    b.call("delete_conversation", json!({ "peerId": a_id }))
        .await
        .unwrap();
    // The session row goes when the client gets to the command.
    eventually(|| async {
        let list = b.call("get_conversations", json!({})).await.ok()?;
        (list.as_array()?.is_empty()).then_some(())
    })
    .await;
    let history = json!({ "peerId": a_id, "limit": 10, "before": null });
    let left = b.call("get_history", history).await.unwrap();
    assert_eq!(left.as_array().map(Vec::len), Some(0));
}

/// A blocked contact stays listed, marked; accepting a known one changes nothing.
async fn block_then_unblock(b: &Desktop, a_id: &str) {
    let blocked = |on: bool| async move {
        let list = b.call("get_conversations", json!({})).await.ok()?;
        (list[0]["blocked"] == on).then_some(())
    };
    let block = json!({ "peerId": a_id, "blocked": true });
    b.call("set_blocked", block).await.unwrap();
    eventually(|| blocked(true)).await;
    let unblock = json!({ "peerId": a_id, "blocked": false });
    b.call("set_blocked", unblock).await.unwrap();
    eventually(|| blocked(false)).await;
    let accept = json!({ "peerId": a_id });
    b.call("accept_contact", accept).await.unwrap();
    let invalid = b.call("accept_contact", json!({ "peerId": "zz" }));
    assert_eq!(invalid.await.unwrap_err(), "invalid peer id");
}

async fn pair(a: &Desktop, b: &Desktop, b_id: &str) {
    let created = once_online(|| a.call("create_link", json!({})))
        .await
        .unwrap();
    let link = text(&created["link_id"]);
    let join = |link: String| json!({ "linkId": link });
    // The host publishes its keys as it comes online; until signaling has
    // stored them, joining finds none.
    let joined = eventually(|| async {
        match once_online(|| b.call("join_link", join(format!("  {link} ")))).await {
            Err(e) if e == "NotFound" => None,
            other => Some(other),
        }
    })
    .await;
    assert_eq!(text(&joined.unwrap()).len(), 64);
    let conversations = eventually(|| async {
        let list = a.call("get_conversations", json!({})).await.unwrap();
        (list.as_array().map(Vec::len) == Some(1)).then_some(list)
    })
    .await;
    assert_eq!(conversations[0]["peer_id"], b_id);
    b.call("join_link", join("not-a-link".into()))
        .await
        .unwrap_err();
}

async fn chat(a: &Desktop, b: &Desktop, a_id: &str, b_id: &str) {
    let sent = a.call("send_message", json!({ "peerId": b_id, "text": "привет" }));
    let msg_id = text(&sent.await.unwrap());
    let received = eventually(|| async {
        let history = json!({ "peerId": a_id, "limit": 10, "before": null });
        let list = b.call("get_history", history).await.unwrap();
        list.as_array()?
            .iter()
            .find(|m| m["text"] == "привет")
            .cloned()
    })
    .await;
    assert_eq!(received["msg_id"], msg_id);
    let read = json!({ "peerId": a_id, "msgIds": [msg_id, "zz"] });
    b.call("mark_read", read).await.unwrap();

    // An answer, then deleting it on this device only.
    let answer = json!({ "peerId": a_id, "text": "и тебе", "replyTo": msg_id });
    let answer_id = text(&b.call("send_message", answer).await.unwrap());
    let history = json!({ "peerId": a_id, "limit": 10, "before": null });
    let mine = eventually(|| async {
        let list = b.call("get_history", history.clone()).await.ok()?;
        list.as_array()?
            .iter()
            .find(|m| m["msg_id"] == answer_id.as_str())
            .cloned()
    })
    .await;
    assert_eq!(mine["reply_to"], msg_id);
    let delete = json!({ "peerId": a_id, "msgId": answer_id, "timestamp": mine["timestamp"] });
    b.call("delete_message", delete.clone()).await.unwrap();
    let left = b.call("get_history", history).await.unwrap();
    assert!(
        left.as_array()
            .unwrap()
            .iter()
            .all(|m| m["msg_id"] != answer_id.as_str())
    );
    b.call("delete_message", delete).await.unwrap_err();
    let bad = json!({ "peerId": a_id, "text": "x", "replyTo": "zz" });
    assert_eq!(
        b.call("send_message", bad).await.unwrap_err(),
        "invalid message id"
    );
}

/// Everything after the microphone: stop, encode, send, and the peer's copy.
async fn send_a_voice_note(a: &Desktop, b: &Desktop, a_id: &str, b_id: &str) {
    let stop = || a.call("voice_stop", json!({ "peerId": b_id }));
    a.state().voice.hold(recorder(4_800));
    assert_eq!(stop().await, Ok(Value::Null), "100 ms is too short to send");

    a.state().voice.hold(recorder(48_000));
    let sent = stop().await.unwrap();
    let duration = sent["duration_ms"].as_u64().unwrap();
    assert!((980..=1020).contains(&duration), "{duration} ms");
    assert_eq!(sent["waveform"].as_array().map(Vec::len), Some(64));

    let kind = eventually(|| async {
        let history = json!({ "peerId": a_id, "limit": 10, "before": null });
        let list = b.call("get_history", history).await.unwrap();
        let note = list
            .as_array()?
            .iter()
            .find(|m| m["msg_id"] == sent["msg_id"])?;
        Some(note["file"]["kind"].clone())
    })
    .await;
    assert_eq!(kind, "voice");
}

/// Sends `poster ‖ video` the way the webview does; returns the file id.
async fn send_video_note_over_ipc(a: &Desktop, b_id: &str, video: &[u8]) -> String {
    let headers: HeaderMap = [
        ("x-peer", b_id),
        ("x-duration-ms", "3000"),
        ("x-poster-len", "2"),
        ("x-mime", "video/webm"),
    ]
    .into_iter()
    .map(|(k, v)| (k.parse().unwrap(), HeaderValue::from_str(v).unwrap()))
    .collect();
    let body = [[0xFF, 0xD8].as_slice(), video].concat();

    let no_body = a.invoke("send_video_note", InvokeBody::default(), headers.clone());
    assert_eq!(
        no_body.await.unwrap_err(),
        "expected the video note as raw bytes or base64 data"
    );
    let not_base64 = a.invoke(
        "send_video_note",
        InvokeBody::Json(json!({ "data": "not base64!" })),
        headers.clone(),
    );
    assert_eq!(
        not_base64.await.unwrap_err(),
        "the video note data is not valid base64"
    );
    // Android's webview sends the same bytes as base64 in JSON.
    let encoded = base64::engine::general_purpose::STANDARD.encode(&body);
    let from_android = a
        .invoke(
            "send_video_note",
            InvokeBody::Json(json!({ "data": encoded })),
            headers.clone(),
        )
        .await
        .unwrap();
    assert_eq!(from_android["duration_ms"], 3000);
    let no_headers = a.invoke(
        "send_video_note",
        InvokeBody::Raw(body.clone()),
        HeaderMap::new(),
    );
    assert_eq!(no_headers.await.unwrap_err(), "missing x-peer");

    let sent = a
        .invoke("send_video_note", InvokeBody::Raw(body), headers)
        .await
        .unwrap();
    assert_eq!(sent["duration_ms"], 3000);
    sent["file_id"].as_str().unwrap().to_owned()
}

async fn play_a_video_note(a: &Desktop, b: &Desktop, b_id: &str) {
    // Larger than one ranged read (1 MiB), so a full read takes several.
    let video: Vec<u8> = (0..1_300_000u32).map(|i| i.to_le_bytes()[0]).collect();
    let file = send_video_note_over_ipc(a, b_id, &video).await;
    let serve = |request| async move { media_scheme::answer(b.app.handle(), &request).await };

    let whole = eventually(|| async {
        let response = serve(media_request(&file, None)).await;
        (response.status() == StatusCode::OK).then_some(response)
    })
    .await;
    assert_eq!(whole.body(), &video);
    assert_eq!(whole.headers()[header::CONTENT_TYPE], "video/webm");

    let ranged = serve(media_request(&file, Some("bytes=10-19"))).await;
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.body().as_slice(), &video[10..20]);
    assert_eq!(
        ranged.headers()[header::CONTENT_RANGE],
        format!("bytes 10-19/{}", video.len())
    );

    let tail = serve(media_request(&file, Some("bytes=-5"))).await;
    assert_eq!(tail.body().as_slice(), &video[video.len() - 5..]);

    let past_the_end = format!("bytes={}-", video.len());
    let statuses = [
        (
            media_request(&file, Some("bytes=5-1")),
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (
            media_request(&file, Some(&past_the_end)),
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (media_request("not-an-id", None), StatusCode::NOT_FOUND),
        (
            media_request(&FileId([9; 16]).to_hex(), None),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (request, expected) in statuses {
        let uri = request.uri().clone();
        assert_eq!(serve(request).await.status(), expected, "{uri}");
    }
}

async fn accept_an_offered_file(a: &Desktop, b: &Desktop, b_id: &str) {
    let src = a.dir.path().join("report.txt");
    std::fs::write(&src, b"quarterly numbers").unwrap();
    std::fs::write(b.dir.path().join("downloads").join("report.txt"), b"older").unwrap();
    let peer = cypher_types::PeerId::from_hex(b_id).unwrap();
    let (_, file_id) = a
        .state()
        .client()
        .await
        .unwrap()
        .send_file(peer, &src, "text/plain", cypher_core::MediaKind::File)
        .await
        .unwrap();

    let offered =
        eventually(|| async { b.state().transfers.offers.contains(&file_id).then_some(()) });
    offered.await;
    let file = json!({ "fileId": file_id.to_hex() });
    b.call("accept_file", file.clone()).await.unwrap();
    // The receiver pre-sizes the file and fills it chunk by chunk.
    let downloads = b.dir.path().join("downloads");
    let saved = downloads.join("report (1).txt");
    eventually(|| async { (std::fs::read(&saved).ok()? == b"quarterly numbers").then_some(()) })
        .await;
    assert_eq!(
        std::fs::read(downloads.join("report.txt")).unwrap(),
        b"older",
        "never overwrites the existing file"
    );
    eventually(|| async {
        (b.call("file_saved", file.clone()).await.ok()? == json!(true)).then_some(())
    })
    .await;
    b.call("accept_file", file.clone()).await.unwrap_err();
    let not_a_picture = media_request(&format!("preview-{}", file_id.to_hex()), None);
    let response = media_scheme::answer(b.app.handle(), &not_a_picture).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    b.call("cancel_transfer", file).await.unwrap();
    let unknown = json!({ "fileId": FileId([7; 16]).to_hex() });
    assert_eq!(
        b.call("file_saved", unknown.clone()).await,
        Ok(json!(false))
    );
    assert_eq!(
        b.call("open_file", unknown.clone()).await,
        Err("not_saved".to_owned())
    );
    assert_eq!(
        b.call("reveal_file", unknown).await,
        Err("not_saved".to_owned())
    );
}

/// Two offers of one name accepted at once land in two files, each whole.
async fn accept_two_files_of_one_name(a: &Desktop, b: &Desktop, b_id: &str) {
    let peer = cypher_types::PeerId::from_hex(b_id).unwrap();
    let client = a.state().client().await.unwrap();
    let mut ids = Vec::new();
    for (folder, body) in [("first", "one twin"), ("second", "the other twin")] {
        let src = a.dir.path().join(folder).join("twin.txt");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, body).unwrap();
        let kind = cypher_core::MediaKind::File;
        let (_, file_id) = client
            .send_file(peer, &src, "text/plain", kind)
            .await
            .unwrap();
        ids.push(file_id);
    }
    eventually(|| async {
        let state = b.state();
        let offers = &state.transfers.offers;
        ids.iter().all(|id| offers.contains(id)).then_some(())
    })
    .await;
    let accept = |id: FileId| b.call("accept_file", json!({ "fileId": id.to_hex() }));
    let (first, second) = tokio::join!(accept(ids[0]), accept(ids[1]));
    first.unwrap();
    second.unwrap();
    let downloads = b.dir.path().join("downloads");
    eventually(|| async {
        let mut saved = [
            std::fs::read_to_string(downloads.join("twin.txt")).ok()?,
            std::fs::read_to_string(downloads.join("twin (1).txt")).ok()?,
        ];
        saved.sort();
        (saved == ["one twin", "the other twin"]).then_some(())
    })
    .await;
}

/// What the file dialog hands back: a path, a `file://` URL, or (Android
/// only) a `content://` URI.
async fn send_picked_files(a: &Desktop, b_id: &str) {
    use crate::commands::transfer::send_pick;
    use tauri_plugin_dialog::FilePath;

    let peer = cypher_types::PeerId::from_hex(b_id).unwrap();
    let client = a.state().client().await.unwrap();
    let handle = a.app.handle();
    let doc = a.dir.path().join("plan.pdf");
    std::fs::write(&doc, b"%PDF").unwrap();

    let (_, _, name, size) = send_pick(handle, &client, peer, FilePath::Path(doc.clone()))
        .await
        .unwrap();
    assert_eq!((name.as_str(), size), ("plan.pdf", 4));
    let url = tauri::Url::from_file_path(&doc).unwrap();
    let (_, _, name, _) = send_pick(handle, &client, peer, FilePath::Url(url))
        .await
        .unwrap();
    assert_eq!(name, "plan.pdf");
    let content: tauri::Url = "content://media/external/file/1".parse().unwrap();
    assert_eq!(
        send_pick(handle, &client, peer, FilePath::Url(content))
            .await
            .unwrap_err(),
        "unsupported file location"
    );
}

/// A pasted picture goes from memory, a dropped one by the path the drop
/// gave; once kept, both show through the preview route, a text file not.
async fn send_and_preview_pictures(a: &Desktop, b: &Desktop, b_id: &str) {
    let mut png = std::io::Cursor::new(Vec::new());
    image::GrayImage::from_pixel(8, 8, image::Luma([200]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let headers: HeaderMap = [
        ("x-peer", b_id.to_owned()),
        ("x-name", "%D1%84%D0%BE%D1%82%D0%BE.png".to_owned()),
        ("x-mime", "image%2Fpng".to_owned()),
    ]
    .iter()
    .map(|(k, v)| (k.parse().unwrap(), HeaderValue::from_str(v).unwrap()))
    .collect();
    let pasted = a
        .invoke("send_bytes", InvokeBody::Raw(png.clone()), headers.clone())
        .await
        .unwrap();
    assert_eq!(pasted["file_name"], "фото.png");
    let too_big = InvokeBody::Raw(vec![0; (50 << 20) + 1]);
    a.invoke("send_bytes", too_big, headers).await.unwrap_err();

    let dropped = a.dir.path().join("drop.png");
    std::fs::write(&dropped, &png).unwrap();
    crate::commands::transfer::files_dropped(
        a.app.handle(),
        vec![dropped, a.dir.path().to_owned()],
    );
    let id = a.state().transfers.dropped.lock().unwrap().id_for_tests();
    let send = json!({ "peerId": b_id, "id": id });
    let sent = a.call("send_dropped", send.clone()).await.unwrap();
    assert_eq!(
        sent.as_array().map(Vec::len),
        Some(1),
        "a folder is skipped"
    );
    a.call("send_dropped", send).await.unwrap_err();

    for info in [pasted, sent[0].clone()] {
        accept_and_preview(b, &info, &png).await;
    }
}

/// Accepts the picture `info` names; its preview is exactly `png`.
async fn accept_and_preview(b: &Desktop, info: &Value, png: &[u8]) {
    let file_id = text(&info["file_id"]);
    let id = FileId::from_hex(&file_id).unwrap();
    eventually(|| async { b.state().transfers.offers.contains(&id).then_some(()) }).await;
    let file = json!({ "fileId": file_id });
    b.call("accept_file", file.clone()).await.unwrap();
    eventually(|| async {
        (b.call("file_saved", file.clone()).await.ok()? == json!(true)).then_some(())
    })
    .await;
    let request = media_request(&format!("preview-{file_id}"), None);
    let preview = media_scheme::answer(b.app.handle(), &request).await;
    assert_eq!(preview.status(), StatusCode::OK);
    assert_eq!(preview.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(preview.body(), png);
}

/// A received program is kept, but never started from the chat.
async fn refuse_to_open_a_program(a: &Desktop, b: &Desktop, b_id: &str) {
    let src = a.dir.path().join("setup.exe");
    std::fs::write(&src, b"MZ").unwrap();
    let peer = cypher_types::PeerId::from_hex(b_id).unwrap();
    let (_, file_id) = a
        .state()
        .client()
        .await
        .unwrap()
        .send_file(
            peer,
            &src,
            "application/octet-stream",
            cypher_core::MediaKind::File,
        )
        .await
        .unwrap();
    eventually(|| async { b.state().transfers.offers.contains(&file_id).then_some(()) }).await;
    let file = json!({ "fileId": file_id.to_hex() });
    b.call("accept_file", file.clone()).await.unwrap();
    eventually(|| async {
        (b.call("file_saved", file.clone()).await.ok()? == json!(true)).then_some(())
    })
    .await;
    assert_eq!(
        b.call("open_file", file).await,
        Err("unsafe_type".to_owned())
    );
}

/// Batch sends, staged copies, waiting for replies and the session's
/// lifecycle, driven against a client that is up but offline (no stack).
mod offline {
    use tauri_plugin_dialog::FilePath;

    use super::*;
    use crate::commands::transfer::send_all;

    /// A desktop with an identity and a client for a gateway nobody runs.
    async fn offline_desktop() -> Desktop {
        let desktop = Desktop::new(cypher_tls::make_client_config());
        desktop.create("alice").await;
        desktop.connect("127.0.0.1:1").await;
        desktop
    }

    /// A file that cannot be sent costs neither the others nor their cards;
    /// when none can be, its error is the answer.
    #[tokio::test]
    async fn a_failed_file_keeps_the_cards_of_the_others() {
        let desktop = offline_desktop().await;
        let client = desktop.state().client().await.unwrap();
        let peer = cypher_types::PeerId([1; 32]);
        let file = |name: &str| desktop.dir.path().join(name);
        std::fs::write(file("first.txt"), b"one").unwrap();
        std::fs::write(file("second.txt"), b"two").unwrap();
        let batch = vec![
            FilePath::Path(file("first.txt")),
            FilePath::Path(file("gone.txt")),
            FilePath::Path(file("second.txt")),
        ];

        let handle = desktop.app.handle();
        let sent = send_all(handle, &client, peer, batch).await.unwrap();
        let sent = serde_json::to_value(sent).unwrap();
        let names: Vec<&str> = sent
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|card| card["file_name"].as_str())
            .collect();
        assert_eq!(names, ["first.txt", "second.txt"]);

        let none = vec![FilePath::Path(file("gone.txt"))];
        send_all(handle, &client, peer, none).await.unwrap_err();
    }

    /// A pasted file is staged as a copy only for sending: whether it went
    /// or not, no copy stays in the app's folder.
    #[tokio::test]
    async fn a_pasted_file_leaves_no_staged_copy() {
        let unconnected = Desktop::new(cypher_tls::make_client_config());
        unconnected.create("alice").await;
        let refused = paste(&unconnected).await.unwrap_err();
        assert_eq!(refused, "not connected");
        assert_eq!(staged(&unconnected), 0, "a copy outlived a failed send");

        let desktop = offline_desktop().await;
        paste(&desktop).await.unwrap();
        assert_eq!(staged(&desktop), 0, "the client took the copy over");
    }

    /// Pastes a few bytes as a picture for a peer, as the webview does.
    async fn paste(desktop: &Desktop) -> Result<Value, Value> {
        let headers: HeaderMap = [
            ("x-peer", cypher_types::PeerId([1; 32]).to_hex()),
            ("x-name", "a.png".to_owned()),
            ("x-mime", "image%2Fpng".to_owned()),
        ]
        .iter()
        .map(|(k, v)| (k.parse().unwrap(), HeaderValue::from_str(v).unwrap()))
        .collect();
        let body = InvokeBody::Raw(b"pasted".to_vec());
        desktop.invoke("send_bytes", body, headers).await
    }

    /// How many staged copies are in the app's folder.
    fn staged(desktop: &Desktop) -> usize {
        let dir = desktop.dir.path().join("data").join("staging");
        std::fs::read_dir(dir).map_or(0, Iterator::count)
    }
}
