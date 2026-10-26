use cypher_core::{Command, CommandId, Event};
use serde::Serialize;
use tauri::State;

use crate::session::{AppState, CmdResult, await_event};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct LinkInfo {
    link_id: String,
}

#[tauri::command]
pub(crate) async fn create_link(state: State<'_, AppState>) -> CmdResult<LinkInfo> {
    let (client, mut events) = state.client_and_events().await?;
    let id = state.command_id();
    client.command(Command::CreateLink { id }).await?;
    await_event(&mut events, created(id)).await
}

/// The answer to `CreateLink` `id`; other events, warnings included, are
/// not its answer.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "picks one reply among all the client's events"
)]
pub(crate) fn created(id: CommandId) -> impl FnMut(&Event) -> Option<CmdResult<LinkInfo>> {
    move |e| match e {
        Event::LinkCreated { id: i, link } if *i == id => Some(Ok(LinkInfo {
            link_id: link.clone(),
        })),
        Event::CreateLinkFailed { id: i, reason } if *i == id => Some(Err((*reason).into())),
        _ => None,
    }
}

/// Resolves to the joined peer id once the session is established.
#[tauri::command]
pub(crate) async fn join_link(state: State<'_, AppState>, link_id: String) -> CmdResult<String> {
    let (client, mut events) = state.client_and_events().await?;
    let id = state.command_id();
    let link = link_id.trim().to_owned();
    client.command(Command::JoinLink { id, link }).await?;
    await_event(&mut events, joined(id)).await
}

/// The answer to `JoinLink` `id`: the contact it added, or why not.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "picks one reply among all the client's events"
)]
pub(crate) fn joined(id: CommandId) -> impl FnMut(&Event) -> Option<CmdResult<String>> {
    move |e| match e {
        Event::PeerAdded {
            peer,
            joined: Some(i),
        } if *i == id => Some(Ok(peer.to_hex())),
        Event::JoinFailed { id: i, reason, .. } if *i == id => Some(Err((*reason).into())),
        _ => None,
    }
}
