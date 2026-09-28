//! Commands driven through Tauri's mock runtime, the way the webview calls
//! them. The two-desktop scenario needs `CYPHER_TEST_REDIS` and
//! `CYPHER_TEST_NATS` (see `tests/e2e`).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cypher_types::FileId;
use serde_json::Value;
use tauri::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};
use tauri::webview::InvokeRequest;
use tauri::{App, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tempfile::TempDir;

use crate::commands::{chat, identity, link, media, qr, settings, transfer};
use crate::media_scheme;
use crate::session::{AppState, Paths};

const PASS: &str = "correct horse battery";
const WAIT: Duration = Duration::from_secs(20);

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
        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![media::send_video_note])
            .build(mock_context(noop_assets()))
            .unwrap();
        app.manage(AppState::new(paths, tls));
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
        let request = InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // The app's own origin, which differs per platform.
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body,
            headers,
            invoke_key: INVOKE_KEY.into(),
        };
        let webview = self.webview.clone();
        tokio::task::spawn_blocking(move || get_ipc_response(&webview, request))
            .await
            .unwrap()
            .map(|body| body.deserialize().unwrap())
    }

    fn state(&self) -> State<'_, AppState> {
        self.app.state()
    }

    async fn create(&self, nickname: &str) -> String {
        identity::create_identity(self.state(), nickname.into(), PASS.into())
            .await
            .unwrap()
    }

    async fn connect(&self, gateway: &str) -> String {
        let handle = self.app.handle().clone();
        settings::connect_to_gateway(handle, self.state(), gateway.into(), false, Vec::new())
            .await
            .unwrap()
    }
}

fn json(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
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
    let state = || desktop.state();
    assert!(!identity::has_identity(state()).await.unwrap());
    assert_eq!(settings::get_nickname(state()).await.unwrap(), None);

    let peer = desktop.create("alice").await;
    assert!(identity::has_identity(state()).await.unwrap());
    assert_eq!(
        settings::get_nickname(state()).await.unwrap().as_deref(),
        Some("alice")
    );

    identity::unlock_identity(state(), "wrong".into())
        .await
        .unwrap_err();
    let (unlocked, nickname) = identity::unlock_identity(state(), PASS.into())
        .await
        .unwrap();
    assert_eq!(
        (unlocked.as_str(), nickname.as_str()),
        (peer.as_str(), "alice")
    );

    identity::export_mnemonic(state(), "wrong".into())
        .await
        .unwrap_err();
    let mnemonic = identity::export_mnemonic(state(), PASS.into())
        .await
        .unwrap();
    let restored = Desktop::new(cypher_tls::make_client_config());
    let imported =
        identity::import_mnemonic(restored.state(), mnemonic, "alice".into(), PASS.into())
            .await
            .unwrap();
    assert_eq!(
        imported, peer,
        "the recovery phrase restores the same identity"
    );
}

#[tokio::test]
async fn commands_fail_cleanly_before_connecting() {
    let desktop = Desktop::new(cypher_tls::make_client_config());
    let state = || desktop.state();
    let handle = || desktop.app.handle().clone();
    let peer = cypher_types::PeerId([1; 32]).to_hex();

    let locked =
        settings::connect_to_gateway(handle(), state(), "localhost:1".into(), false, vec![]);
    assert_eq!(locked.await.unwrap_err(), "identity is locked");
    desktop.create("bob").await;
    let bad_addr = settings::connect_to_gateway(handle(), state(), "no port".into(), false, vec![]);
    bad_addr.await.unwrap_err();

    assert_eq!(
        identity::get_conversations(state()).await.unwrap_err(),
        "not connected"
    );
    assert_eq!(
        chat::send_message(state(), peer.clone(), "x".into())
            .await
            .unwrap_err(),
        "not connected"
    );
    assert_eq!(
        chat::send_message(state(), "zz".into(), "x".into())
            .await
            .unwrap_err(),
        "invalid peer id"
    );
    identity::get_history(state(), "zz".into(), None, 10)
        .await
        .unwrap_err();
    identity::clear_chat_history(state()).await.unwrap_err();
    link::create_link(state()).await.unwrap_err();
    transfer::accept_file(state(), FileId([2; 16]).to_hex())
        .await
        .unwrap_err();
    transfer::cancel_transfer(state(), "zz".into())
        .await
        .unwrap_err();
    assert_eq!(
        media::voice_stop(state(), peer).await.unwrap_err(),
        "not recording"
    );
    media::voice_cancel(state()).await.unwrap();
}

#[tokio::test]
async fn qr_codes_are_png_data_uris() {
    let uri = qr::generate_qr("cypher-link".into()).await.unwrap();
    assert!(uri.starts_with("data:image/png;base64,"));
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
    play_a_video_note(&a, &b, &b_id).await;
    accept_an_offered_file(&a, &b, &b_id).await;

    settings::apply_anonymous_settings(a.app.handle().clone(), a.state(), false, vec![" ".into()])
        .await
        .unwrap();
    identity::clear_chat_history(a.state()).await.unwrap();
    stack.stop().await;
}

async fn pair(a: &Desktop, b: &Desktop, b_id: &str) {
    let created = once_online(|| link::create_link(a.state())).await.unwrap();
    let link = json(created)["link_id"].as_str().unwrap().to_owned();
    let joined = once_online(|| link::join_link(b.state(), format!("  {link} ")))
        .await
        .unwrap();
    assert_eq!(joined.len(), 64);
    let conversations = eventually(|| async {
        let list = json(identity::get_conversations(a.state()).await.unwrap());
        (list.as_array().map(Vec::len) == Some(1)).then_some(list)
    })
    .await;
    assert_eq!(conversations[0]["peer_id"], b_id);
    link::join_link(b.state(), "not-a-link".into())
        .await
        .unwrap_err();
}

async fn chat(a: &Desktop, b: &Desktop, a_id: &str, b_id: &str) {
    let msg_id = chat::send_message(a.state(), b_id.into(), "привет".into())
        .await
        .unwrap();
    let received = eventually(|| async {
        let history = identity::get_history(b.state(), a_id.into(), None, 10)
            .await
            .unwrap();
        history.into_iter().find(|m| m.text == "привет")
    })
    .await;
    assert_eq!(received.msg_id, msg_id);
    chat::mark_read(b.state(), a_id.into(), vec![msg_id, "zz".into()])
        .await
        .unwrap();
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

    let json_body = a.invoke("send_video_note", InvokeBody::default(), headers.clone());
    assert_eq!(json_body.await.unwrap_err(), "expected a binary body");
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
    let video: Vec<u8> = (0..200_000u32).map(|i| i.to_le_bytes()[0]).collect();
    let file = send_video_note_over_ipc(a, b_id, &video).await;
    let client = b.state().client().await.unwrap();

    let whole = eventually(|| async {
        let response = media_scheme::serve(&client, &media_request(&file, None)).await;
        (response.status() == StatusCode::OK).then_some(response)
    })
    .await;
    assert_eq!(whole.body(), &video);
    assert_eq!(whole.headers()[header::CONTENT_TYPE], "video/webm");

    let ranged = media_scheme::serve(&client, &media_request(&file, Some("bytes=10-19"))).await;
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.body().as_slice(), &video[10..20]);
    assert_eq!(
        ranged.headers()[header::CONTENT_RANGE],
        "bytes 10-19/200000"
    );

    let tail = media_scheme::serve(&client, &media_request(&file, Some("bytes=-5"))).await;
    assert_eq!(tail.body().as_slice(), &video[video.len() - 5..]);

    let statuses = [
        (
            media_request(&file, Some("bytes=5-1")),
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (
            media_request(&file, Some("bytes=999999-")),
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
        assert_eq!(
            media_scheme::serve(&client, &request).await.status(),
            expected,
            "{uri}"
        );
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

    let offered = eventually(|| async {
        b.state()
            .offers
            .lock()
            .unwrap()
            .contains_key(&file_id)
            .then_some(())
    });
    offered.await;
    transfer::accept_file(b.state(), file_id.to_hex())
        .await
        .unwrap();
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
    transfer::accept_file(b.state(), file_id.to_hex())
        .await
        .unwrap_err();
    transfer::cancel_transfer(b.state(), file_id.to_hex())
        .await
        .unwrap();
}
