//! Restoring from stored state that this release cannot fully read.

use cypher_core::{Core, CoreError, Effect, Event, FailReason, Snapshot, Table, Vault};
use cypher_crypto::IdentitySeed;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const NOW: u64 = 1_700_000_000_000;

fn restore(seed: &IdentitySeed, snapshot: &Snapshot) -> Result<Vec<Effect>, CoreError> {
    Core::restore(seed, snapshot, NOW, ChaCha20Rng::seed_from_u64(1)).map(|(_, effects)| effects)
}

/// A peer row sealed as record `version`, as another release would write it.
fn peer_row(seed: &IdentitySeed, version: u8) -> (Vec<u8>, Vec<u8>) {
    let key = vec![7; 32];
    let vault = Vault::new(seed.derive_storage_key());
    let mut rng = ChaCha20Rng::seed_from_u64(2);
    let value = vault.seal_bytes(Table::Peers, &key, &[version, 0, 0], &mut rng);
    (key, value)
}

#[test]
fn a_damaged_record_is_skipped_and_reported() {
    let seed = IdentitySeed::generate();
    let snapshot = Snapshot {
        peers: vec![peer_row(&seed, 1), (vec![8; 32], vec![0; 40])],
        ..Snapshot::default()
    };
    let effects = restore(&seed, &snapshot).expect("one bad row does not stop the restore");
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Emit(Event::Warning {
            reason: FailReason::Corrupted
        })
    )));
}

#[test]
fn state_from_a_newer_release_is_refused() {
    let seed = IdentitySeed::generate();
    let snapshot = Snapshot {
        peers: vec![peer_row(&seed, u8::MAX)],
        ..Snapshot::default()
    };
    assert!(matches!(
        restore(&seed, &snapshot),
        Err(CoreError::NewerStorage)
    ));
}

#[test]
fn a_clean_store_restores_without_warnings() {
    let seed = IdentitySeed::generate();
    let effects = restore(&seed, &Snapshot::default()).unwrap();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Emit(Event::Warning { .. })))
    );
}
