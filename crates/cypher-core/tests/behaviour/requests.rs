//! Requests to the server and what their failures mean: an error code, no
//! answer in time, or the connection going while they wait.

use bytes::Bytes;
use cypher_core::{Command, CommandId, Event, FailReason};
use cypher_types::{Addr, DeviceId};
use cypher_wire::{ClientMsg, ErrorCode, Frame, ServerMsg};

use super::{events_since, rows};
use crate::harness::{Fault, Rule, Seen, World};
use crate::{A, B, C, D, paired};

/// How long a request waits for its answer.
const TIMEOUT_MS: u64 = 15_000;
/// Time between two ticks of the test world.
const TICK_MS: u64 = 500;

/// Every error code a client tells apart, as it decodes them off the wire:
/// a code added there is covered here without being listed.
fn codes() -> Vec<ErrorCode> {
    let frame = Frame::new(
        0,
        ServerMsg::Error {
            code: ErrorCode::NotFound,
        },
    )
    .encode();
    let mut codes = Vec::new();
    for n in 0..=u8::MAX {
        // The code is the frame's last two bytes.
        let mut raw = frame.to_vec();
        let at = raw.len() - 2;
        raw[at..].copy_from_slice(&u16::from(n).to_le_bytes());
        if let Ok(Frame {
            msg: ServerMsg::Error { code },
            ..
        }) = Frame::<ServerMsg>::decode(Bytes::from(raw))
            && !codes.contains(&code)
        {
            codes.push(code);
        }
    }
    assert!(codes.len() >= 9, "{codes:?}");
    codes
}

/// What a server error tells the user about the request it answers: the
/// server's own trouble, retryable or not, or what the request asked for.
#[test]
fn server_errors_tell_the_user_what_they_mean() {
    for code in codes() {
        let expected = match code {
            ErrorCode::NotFound => FailReason::NotFound,
            ErrorCode::Unauthorized => FailReason::Unauthorized,
            ErrorCode::Conflict => FailReason::Rejected,
            // Try again later.
            ErrorCode::RateLimited | ErrorCode::Unavailable => FailReason::Offline,
            ErrorCode::BadRequest | ErrorCode::TooLarge | ErrorCode::Internal => {
                FailReason::ServerError
            }
            ErrorCode::UnsupportedVersion => FailReason::UpdateRequired,
        };
        let mut w = World::new(1);
        w.server.faults.push(Rule::always(
            |m| matches!(m, ClientMsg::CreateLink),
            Fault::Error(code),
        ));
        let from = w.clients[A].events.len();
        w.command(A, Command::CreateLink { id: CommandId(0) });
        let told: Vec<&Event> = events_since(&w, A, from).iter().collect();
        assert_eq!(
            told,
            [&Event::CreateLinkFailed {
                id: CommandId(0),
                reason: expected
            }],
            "{code:?} means {expected:?}"
        );
    }
}

/// A request the user waits for, and what A tells when it fails.
struct Visible {
    name: &'static str,
    world: fn() -> World,
    /// Has A send the request, which the server fails as `fault` says.
    send: fn(&mut World, Fault),
    told: fn(&Event, FailReason) -> bool,
}

fn join(w: &mut World, picks: fn(&ClientMsg) -> bool, fault: Fault) {
    let link = w.create_link(B);
    w.server.faults.push(Rule::always(picks, fault));
    w.command(
        A,
        Command::JoinLink {
            id: CommandId(0),
            link,
        },
    );
}

/// A asks to hand its identity to a new device; `fault` hits the request
/// `picks` chooses among those the hand-over makes.
fn link_device(w: &mut World, fault: Fault, picks_listing: bool) {
    let phone = w.provision("phone");
    let offer = w.offer(phone);
    let temp = w.clients[phone].provision.as_ref().unwrap().offer().temp;
    let rule = if picks_listing {
        Rule::always(|m| matches!(m, ClientMsg::PublishDevices { .. }), fault)
    } else {
        Rule::always(
            move |m| matches!(m, ClientMsg::Send { to, .. } if *to == temp),
            fault,
        )
    };
    w.server.faults.push(rule);
    w.command(
        A,
        Command::LinkDevice {
            id: CommandId(0),
            offer,
            history: false,
        },
    );
}

fn joining_world() -> World {
    World::new(2)
}

fn lone_world() -> World {
    World::new(1)
}

fn visible_requests() -> [Visible; 7] {
    let warning = |e: &Event, r| matches!(e, Event::Warning { reason } if *reason == r);
    let not_created =
        |e: &Event, r| matches!(e, Event::CreateLinkFailed { reason, .. } if *reason == r);
    let join_failed = |e: &Event, r| matches!(e, Event::JoinFailed { reason, .. } if *reason == r);
    let link_failed = |e: &Event, r| matches!(e, Event::LinkFailed { reason, .. } if *reason == r);
    [
        Visible {
            name: "an invite",
            world: lone_world,
            send: |w, fault| {
                w.server
                    .faults
                    .push(Rule::always(|m| matches!(m, ClientMsg::CreateLink), fault));
                w.command(A, Command::CreateLink { id: CommandId(0) });
            },
            told: not_created,
        },
        Visible {
            name: "who made an invite",
            world: joining_world,
            send: |w, fault| join(w, |m| matches!(m, ClientMsg::ResolveLink { .. }), fault),
            told: join_failed,
        },
        Visible {
            name: "the host's devices",
            world: joining_world,
            send: |w, fault| join(w, |m| matches!(m, ClientMsg::FetchDevices { .. }), fault),
            told: join_failed,
        },
        Visible {
            name: "the host's keys",
            world: joining_world,
            send: |w, fault| join(w, |m| matches!(m, ClientMsg::FetchKeys { .. }), fault),
            told: join_failed,
        },
        Visible {
            name: "the hand-over to a new device",
            world: lone_world,
            send: |w, fault| link_device(w, fault, false),
            told: link_failed,
        },
        Visible {
            name: "listing a new device",
            world: lone_world,
            send: |w, fault| link_device(w, fault, true),
            told: link_failed,
        },
        Visible {
            name: "reading the inbox",
            world: lone_world,
            send: |w, fault| {
                w.server.faults.push(Rule::always(
                    |m| matches!(m, ClientMsg::InboxFetch { close: false, .. }),
                    fault,
                ));
                w.command(A, Command::FetchInbox);
            },
            told: warning,
        },
    ]
}

/// The failure events among `events`, whatever their reason.
fn failures(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::Warning { .. }
                    | Event::CreateLinkFailed { .. }
                    | Event::JoinFailed { .. }
                    | Event::LinkFailed { .. }
                    | Event::PushUnavailable
            )
        })
        .collect()
}

/// A request the server never answers fails when its time is up, not
/// before; the user is told so.
#[test]
fn an_unanswered_request_fails_when_its_time_is_up() {
    for kind in visible_requests() {
        let mut w = (kind.world)();
        let from = w.clients[A].events.len();
        (kind.send)(&mut w, Fault::Ignore);
        w.advance(TIMEOUT_MS - TICK_MS);
        assert!(
            failures(events_since(&w, A, from)).is_empty(),
            "{}: not before its time",
            kind.name
        );
        w.advance(TICK_MS);
        let told = failures(events_since(&w, A, from));
        assert!(!told.is_empty(), "{}: told", kind.name);
        assert!(
            told.iter().all(|e| (kind.told)(e, FailReason::Timeout)),
            "{}: {told:?}",
            kind.name
        );
    }
}

/// A request the server refuses fails at once, and the user is told why.
#[test]
fn a_refused_request_fails_at_once() {
    for kind in visible_requests() {
        let mut w = (kind.world)();
        let from = w.clients[A].events.len();
        (kind.send)(&mut w, Fault::Error(ErrorCode::Internal));
        let told = failures(events_since(&w, A, from));
        assert!(!told.is_empty(), "{}: told", kind.name);
        assert!(
            told.iter().all(|e| (kind.told)(e, FailReason::ServerError)),
            "{}: {told:?}",
            kind.name
        );
    }
}

/// A host that has no device list is joined at its first device.
#[test]
fn a_host_without_a_device_list_is_joined_at_its_first_device() {
    let mut w = joining_world();
    join(
        &mut w,
        |m| matches!(m, ClientMsg::FetchDevices { .. }),
        Fault::Error(ErrorCode::NotFound),
    );
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::PeerAdded {
            joined: Some(_),
            ..
        }
    )));
    assert!(failures(&w.clients[A].events).is_empty());
}

/// Which requests a test fails.
type Picks = fn(&ClientMsg) -> bool;

/// Has B, away, come back to a message in its inbox while the server fails
/// what `picks` chooses as `fault` says; B's events from then on.
fn back_to_the_inbox(picks: Picks, fault: Fault) -> (World, usize) {
    let mut w = paired();
    w.disconnect(B);
    w.send_text(A, B, "while you were away");
    w.server.faults.push(Rule::always(picks, fault));
    let from = w.clients[B].events.len();
    w.connect(B);
    w.advance(60 * 60_000 + TIMEOUT_MS);
    (w, from)
}

/// Requests whose failure the user need not hear of: publishing keys,
/// telling the gateway the access tokens, checking this device's own list.
/// Their failure tells nothing, and what waited is shown once.
#[test]
fn some_requests_fail_quietly() {
    let quiet: [(&str, Picks); 3] = [
        ("keys", |m| matches!(m, ClientMsg::PublishKeys { .. })),
        ("access tokens", |m| {
            matches!(m, ClientMsg::SetAccess { .. })
        }),
        ("own devices", |m| {
            matches!(m, ClientMsg::FetchDevices { .. })
        }),
    ];
    for (name, picks) in quiet {
        for fault in [Fault::Ignore, Fault::Error(ErrorCode::Internal)] {
            let (w, from) = back_to_the_inbox(picks, fault);
            let told = failures(events_since(&w, B, from));
            assert!(told.is_empty(), "{name}, {fault:?}: {told:?}");
            assert_eq!(w.texts(B), ["while you were away"], "{name}, {fault:?}");
        }
    }
}

/// The confirmation of an inbox read is lost: the server hands the same
/// items out again, which is no news to the user, nor a reason to start
/// the session afresh.
#[test]
fn items_read_again_after_a_lost_confirmation_are_no_news() {
    for fault in [Fault::Ignore, Fault::Error(ErrorCode::Internal)] {
        let (w, from) = back_to_the_inbox(|m| matches!(m, ClientMsg::InboxAck { .. }), fault);
        let key_lookups = w
            .server
            .served
            .iter()
            .filter(|(i, _, m)| *i == B && matches!(m, ClientMsg::FetchKeys { .. }))
            .count();
        assert_eq!(key_lookups, 1, "{fault:?}: only to join");
        let told = failures(events_since(&w, B, from));
        assert!(told.is_empty(), "{fault:?}: {told:?}");
        assert_eq!(w.texts(B), ["while you were away"], "{fault:?}");
    }
}

/// With the driver's relay down, a server that cannot say where its relay
/// is, or does not answer, is used without one: what would go through the
/// relay goes over the session, at once after a refusal, once the request
/// times out otherwise.
#[test]
fn without_word_of_the_relay_requests_go_over_the_session() {
    for (fault, after) in [
        (Fault::Error(ErrorCode::Internal), 0),
        (Fault::Ignore, TIMEOUT_MS),
    ] {
        let mut w = World::new(1);
        w.server
            .faults
            .push(Rule::always(|m| matches!(m, ClientMsg::Bootstrap), fault));
        w.relay_up = false;
        w.disconnect(A);
        w.connect(A);
        w.run();
        if after > 0 {
            w.advance(after - TICK_MS);
            assert_eq!(w.server.session_inbox_ops, 0, "{fault:?}: waits first");
            w.advance(TICK_MS);
        }
        assert!(w.server.session_inbox_ops > 0, "{fault:?}");
    }
}

/// Whether a failed push request tells that push is unavailable: when the
/// server has none or broke, and for the key also when it is busy.
#[test]
fn failed_push_requests_say_push_is_unavailable_only_when_it_is() {
    for code in codes() {
        let mut w = paired();
        w.server.faults.push(Rule::always(
            |m| matches!(m, ClientMsg::PushKey),
            Fault::Error(code),
        ));
        w.command(B, Command::EnablePush);
        let unavailable = w.has_event(B, |e| matches!(e, Event::PushUnavailable));
        let expected = !matches!(
            code,
            ErrorCode::Unauthorized | ErrorCode::Conflict | ErrorCode::UnsupportedVersion
        );
        assert_eq!(unavailable, expected, "the key, {code:?}");

        let mut w = paired();
        w.command(B, Command::EnablePush);
        w.server.faults.push(Rule::always(
            |m| matches!(m, ClientMsg::PushRegister { .. }),
            Fault::Error(code),
        ));
        w.command(
            B,
            Command::RegisterPush {
                endpoint: "https://ntfy.sh/up-b".into(),
                p256dh: [4; 65],
                auth: [1; 16],
            },
        );
        let unavailable = w.has_event(B, |e| matches!(e, Event::PushUnavailable));
        let expected = matches!(
            code,
            ErrorCode::NotFound | ErrorCode::BadRequest | ErrorCode::TooLarge | ErrorCode::Internal
        );
        assert_eq!(unavailable, expected, "the registration, {code:?}");
    }
    let mut w = paired();
    w.server.faults.push(Rule::always(
        |m| matches!(m, ClientMsg::PushKey),
        Fault::Ignore,
    ));
    w.command(B, Command::EnablePush);
    w.advance(TIMEOUT_MS);
    assert!(!w.has_event(B, |e| matches!(e, Event::PushUnavailable)));
}

/// When the connection goes, every request still waiting for an answer
/// fails as offline, in the order it was made, before the user hears the
/// connection is gone.
#[test]
fn requests_waiting_for_an_answer_fail_when_the_connection_goes() {
    let mut w = joining_world();
    let link = w.create_link(B);
    w.server.faults.push(Rule::always(
        |m| {
            matches!(
                m,
                ClientMsg::CreateLink
                    | ClientMsg::ResolveLink { .. }
                    | ClientMsg::InboxFetch { .. }
            )
        },
        Fault::Ignore,
    ));
    w.command(A, Command::CreateLink { id: CommandId(0) });
    w.command(
        A,
        Command::JoinLink {
            id: CommandId(0),
            link,
        },
    );
    w.command(A, Command::FetchInbox);
    let from = w.clients[A].events.len();
    w.disconnect(A);
    let told = events_since(&w, A, from);
    let offline = FailReason::Offline;
    assert!(
        matches!(told.first(), Some(Event::CreateLinkFailed { reason, .. }) if *reason == offline),
        "{told:?}"
    );
    assert!(
        matches!(told.get(1), Some(Event::JoinFailed { reason, .. }) if *reason == offline),
        "{told:?}"
    );
    let (last, between) = told.split_last().unwrap();
    assert_eq!(last, &Event::Disconnected);
    assert!(between.len() > 2, "the inbox reads too: {told:?}");
    assert!(
        between[2..]
            .iter()
            .all(|e| matches!(e, Event::Warning { reason } if *reason == offline))
    );
}

/// Requests made while waiting for the relay go through it in the order
/// they were made, once it is up.
#[test]
fn requests_waiting_for_the_relay_go_in_the_order_made() {
    let mut w = World::new(4);
    let links: Vec<String> = [B, C, D].into_iter().map(|h| w.create_link(h)).collect();
    w.relay_up = false;
    w.disconnect(A);
    w.connect(A);
    w.run();
    let start = w.server.served.len();
    for link in &links {
        w.command(
            A,
            Command::JoinLink {
                id: CommandId(0),
                link: link.clone(),
            },
        );
    }
    let resolved = |w: &World| -> Vec<String> {
        w.server.served[start..]
            .iter()
            .filter_map(|(from, _, msg)| match msg {
                ClientMsg::ResolveLink { link } if *from == A => Some(link.as_str().to_owned()),
                _ => None,
            })
            .collect()
    };
    assert!(resolved(&w).is_empty(), "they wait for the relay");
    w.set_relay(A, true);
    let codes: Vec<String> = links
        .iter()
        .map(|l| l.split_once('-').map_or(l.as_str(), |(c, _)| c).to_owned())
        .collect();
    assert_eq!(resolved(&w), codes);
    assert_eq!(w.server.session_intro_ops, 0);
}

/// When client `i` asked the server for `addr`'s keys.
fn key_requests(w: &World, i: usize, addr: Addr) -> Vec<u64> {
    let log = w.log.as_ref().expect("the log is kept");
    log.iter()
        .filter(|batch| batch.client == i)
        .flat_map(|batch| {
            batch.effects.iter().filter_map(move |e| {
                let frame = match e {
                    Seen::Anonymous(blob) => w.open_anonymous(blob),
                    Seen::Transmit(raw) => Frame::<ClientMsg>::decode(raw.clone()).ok(),
                    _ => None,
                }?;
                matches!(frame.msg, ClientMsg::FetchKeys { peer, device }
                    if Addr::new(peer, device) == addr)
                .then_some(batch.at)
            })
        })
        .collect()
}

/// The keys of a contact's device could not be had: the user is not told,
/// and they are asked for again ten minutes later, not sooner.
#[test]
fn keys_that_could_not_be_had_are_asked_for_again_ten_minutes_on() {
    const REPAIR_MS: u64 = 10 * 60_000;
    let mut w = World::new(2);
    let b2 = w.add_device(B, 2);
    w.advance(60 * 60_000);
    w.disconnect(b2);
    let to_b2 = Addr::new(w.peer(B), DeviceId(2));
    w.server.faults.push(Rule::always(
        move |m| matches!(m, ClientMsg::FetchKeys { peer, device } if Addr::new(*peer, *device) == to_b2),
        Fault::Error(ErrorCode::Internal),
    ));
    w.pair(B, A);
    w.log = Some(Vec::new());
    let from = w.clients[A].events.len();
    let id = w.send_text(A, B, "to both");
    w.advance(35 * 60_000);
    let asked = key_requests(&w, A, to_b2);
    assert!(asked.len() >= 3, "{asked:?}");
    for gap in asked.windows(2).map(|t| t[1] - t[0]) {
        assert!(
            (REPAIR_MS..=REPAIR_MS + 60_000).contains(&gap),
            "asked again after {gap} ms: {asked:?}"
        );
    }
    assert!(failures(events_since(&w, A, from)).is_empty());
    let row = rows::outbox_row(&w, A, id).unwrap();
    assert!(
        row.targets.iter().any(|t| t.device == DeviceId(2)),
        "still to go"
    );
}
