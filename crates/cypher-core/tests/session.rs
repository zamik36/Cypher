//! How the core reacts to what the server says about the session itself.

use cypher_core::{Core, Effect, Event, FailReason, Input, Snapshot};
use cypher_crypto::IdentitySeed;
use cypher_wire::{ErrorCode, Frame, ServerMsg};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const NOW: u64 = 1_700_000_000_000;

/// A server on another protocol version cannot be reached by reconnecting:
/// the client stops and asks for an update instead of retrying forever.
#[test]
fn an_unsupported_version_stops_the_client_and_asks_for_an_update() {
    let seed = IdentitySeed::generate();
    let (mut core, _) = Core::restore(
        &seed,
        &Snapshot::default(),
        NOW,
        ChaCha20Rng::seed_from_u64(1),
    )
    .unwrap();
    core.handle(Input::Connected, NOW);
    let refusal = Frame::new(
        0,
        ServerMsg::Error {
            code: ErrorCode::UnsupportedVersion,
        },
    )
    .encode();
    let effects = core.handle(Input::Frame(refusal), NOW);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Emit(Event::Warning {
            reason: FailReason::UpdateRequired
        })
    )));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Disconnect { reconnect: false }))
    );
}
