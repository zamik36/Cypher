use cypher_core::{Command, Event};
use serde::Serialize;
use tauri::State;

use crate::session::{AppState, CmdResult, await_event, err};

#[derive(Serialize)]
pub struct LinkInfo {
    link_id: String,
}

#[tauri::command]
pub async fn create_link(state: State<'_, AppState>) -> CmdResult<LinkInfo> {
    let (client, mut events) = state.client_and_events().await?;
    client.command(Command::CreateLink).await.map_err(err)?;
    await_event(&mut events, |e| match e {
        Event::LinkCreated { link } => Some(Ok(LinkInfo {
            link_id: link.clone(),
        })),
        Event::Warning { reason } => Some(Err(format!("{reason:?}"))),
        _ => None,
    })
    .await
}

/// Resolves to the joined peer id once the session is established.
#[tauri::command]
pub async fn join_link(state: State<'_, AppState>, link_id: String) -> CmdResult<String> {
    let (client, mut events) = state.client_and_events().await?;
    let link = link_id.trim().to_owned();
    client
        .command(Command::JoinLink { link: link.clone() })
        .await
        .map_err(err)?;
    await_event(&mut events, |e| match e {
        Event::PeerAdded {
            peer,
            initiated_by_us: true,
        } => Some(Ok(peer.to_hex())),
        Event::JoinFailed { link: l, reason } if *l == link => Some(Err(format!("{reason:?}"))),
        _ => None,
    })
    .await
}
