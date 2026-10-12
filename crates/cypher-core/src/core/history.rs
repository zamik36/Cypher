//! Handing the message history to a device just linked to this identity.
//!
//! The device that links another may promise it the history. Once the new
//! device asks its siblings for what they know, the promising one has its
//! driver build an archive ([`crate::history`]) and sends it as a file over
//! their session. The new device takes one such archive, and only while it
//! is new; its driver then takes in the rows it does not have yet.

use cypher_crypto::FileKey;
use cypher_types::{Addr, DeviceId, FileId};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::Core;
use crate::api::{Effect, Event, FailReason, MediaKind};
use crate::envelope::{Body, FILE_CHUNK_SIZE, FileDesc, SyncBody};
use crate::store::{META_HISTORY, Table};
use crate::transfer::{Incoming, Outgoing};

/// How long a link's promise of the history waits for the new device.
const GRANT_TTL_MS: u64 = 24 * 60 * 60_000;
const ARCHIVE_NAME: &str = "history";
const ARCHIVE_MIME: &str = "application/x-cypher-history";

/// Whether this device takes the history from a sibling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Inbound {
    #[default]
    Closed,
    /// It just asked its siblings for what they know.
    Open,
    /// The archive arrived; the driver is taking it in.
    Importing(FileId),
}

/// Where the history stands on this device, kept in `meta`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct History {
    /// Devices linked from here that are to get the history, and when.
    grants: Vec<(DeviceId, u64)>,
    /// The archive the driver is building, and for which device.
    building: Option<(DeviceId, FileId)>,
    inbound: Inbound,
}

impl crate::Record for History {
    const VERSION: u8 = 1;
}

impl<R: CryptoRngCore> Core<R> {
    /// The device being linked is to get the history once it asks.
    pub(super) fn grant_history(&mut self, device: DeviceId) {
        let now = self.now;
        self.history.grants.retain(|(d, _)| *d != device);
        self.history.grants.push((device, now));
        self.persist_history();
    }

    /// Linking it failed: it will not ask.
    pub(super) fn revoke_history(&mut self, device: DeviceId) {
        let before = self.history.grants.len();
        self.history.grants.retain(|(d, _)| *d != device);
        if self.history.grants.len() != before {
            self.persist_history();
        }
    }

    /// This device, new, asked its siblings: it takes one history.
    pub(super) fn open_history(&mut self) {
        self.history.inbound = Inbound::Open;
        self.persist_history();
    }

    /// `device` asked what this one knows: if it was promised the history,
    /// the driver builds it.
    pub(super) fn serve_history(&mut self, device: DeviceId) {
        let now = self.now;
        self.history
            .grants
            .retain(|(_, at)| now.saturating_sub(*at) < GRANT_TTL_MS);
        let granted = self.history.grants.iter().position(|(d, _)| *d == device);
        let Some(granted) = granted.filter(|_| self.history.building.is_none()) else {
            return;
        };
        self.history.grants.remove(granted);
        let mut id = [0u8; 16];
        self.rng.fill_bytes(&mut id);
        let file_id = FileId(id);
        self.history.building = Some((device, file_id));
        self.persist_history();
        let partial = self.partial_media();
        self.effects.push(Effect::BuildHistory { file_id, partial });
    }

    /// Voice and video notes still on their way: their copies are not whole.
    fn partial_media(&self) -> Vec<FileId> {
        let outgoing = self
            .outgoing
            .iter()
            .filter(|(_, o)| o.kind.is_media())
            .map(|(id, _)| *id);
        let incoming = self
            .incoming
            .iter()
            .filter(|(_, i)| i.kind.is_media())
            .map(|(id, _)| *id);
        outgoing.chain(incoming).collect()
    }

    /// The driver built the archive (or could not): it goes to the device
    /// it was built for.
    pub(super) fn on_history_built(&mut self, file_id: FileId, size: Option<u64>) {
        let Some((device, _)) = self.history.building.filter(|(_, id)| *id == file_id) else {
            return;
        };
        self.history.building = None;
        self.persist_history();
        let Some(size) = size else {
            return self.emit(Event::HistoryFailed {
                file_id,
                outgoing: true,
                reason: FailReason::SourceUnavailable,
            });
        };
        let desc = FileDesc {
            file_id,
            name: ARCHIVE_NAME.to_owned(),
            mime: ARCHIVE_MIME.to_owned(),
            size,
            chunk_size: FILE_CHUNK_SIZE,
            key: *FileKey::random(&mut self.rng).as_bytes(),
            inline: None,
        };
        let own = self.peer_id;
        let out = Outgoing::new(own, desc.clone(), MediaKind::File);
        self.persist_transfer_record(file_id, &out.to_record());
        self.outgoing.insert(file_id, out);
        self.send_control_to(
            Addr::new(own, device),
            Body::Sync(SyncBody::History { desc }),
        );
    }

    /// A sibling offers its history: taken once, while this device is new.
    pub(super) fn on_history_offer(&mut self, sender: Addr, desc: FileDesc) {
        let file_id = desc.file_id;
        let known = self.incoming.contains_key(&file_id) || self.outgoing.contains_key(&file_id);
        if self.history.inbound != Inbound::Open || known {
            return;
        }
        self.history.inbound = Inbound::Closed;
        self.persist_history();
        let inc = Incoming::new(sender, desc, MediaKind::File);
        self.persist_transfer_record(file_id, &inc.to_record());
        self.incoming.insert(file_id, inc);
        self.accept_file(&file_id);
    }

    /// The archive arrived whole: the driver takes it in.
    pub(super) fn import_history(&mut self, file_id: FileId) {
        self.history.inbound = Inbound::Importing(file_id);
        self.persist_history();
        self.effects.push(Effect::ImportHistory { file_id });
    }

    pub(super) fn on_history_imported(&mut self, file_id: FileId, ok: bool) {
        if self.history.inbound != Inbound::Importing(file_id) {
            return;
        }
        self.history.inbound = Inbound::Closed;
        self.persist_history();
        self.emit(if ok {
            Event::HistoryDone {
                file_id,
                outgoing: false,
            }
        } else {
            Event::HistoryFailed {
                file_id,
                outgoing: false,
                reason: FailReason::Corrupted,
            }
        });
    }

    /// Stops building, sending or receiving the history.
    pub(super) fn cancel_history(&mut self) {
        if let Some((_, file_id)) = self.history.building.take() {
            self.persist_history();
            self.emit(Event::HistoryFailed {
                file_id,
                outgoing: true,
                reason: FailReason::Cancelled,
            });
        }
        let own = self.peer_id;
        let transfers: Vec<FileId> = self
            .outgoing
            .iter()
            .filter(|(_, o)| o.peer == own)
            .map(|(id, _)| *id)
            .chain(
                self.incoming
                    .iter()
                    .filter(|(_, i)| i.peer == own)
                    .map(|(id, _)| *id),
            )
            .collect();
        for file_id in transfers {
            self.cancel_transfer(&file_id);
        }
    }

    /// After a restart, the driver is asked again for what it was doing:
    /// building and taking in are both done from scratch.
    pub(super) fn resume_history(&mut self) {
        if let Some((_, file_id)) = self.history.building {
            let partial = self.partial_media();
            self.effects.push(Effect::BuildHistory { file_id, partial });
        }
        if let Inbound::Importing(file_id) = self.history.inbound {
            self.effects.push(Effect::ImportHistory { file_id });
        }
    }

    fn persist_history(&mut self) {
        let op = self.vault.put(
            Table::Meta,
            META_HISTORY.to_vec(),
            &self.history,
            &mut self.rng,
        );
        self.persist(op);
    }
}
