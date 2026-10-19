//! What belongs to the unlocked identity besides its client, each part
//! owning its state and how it ends: all of it goes when the identity is
//! locked, erased or replaced, so nothing carries over to the next one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};

use cypher_client::Provisioning;
use cypher_core::Event;
use cypher_media::Recorder;
use cypher_types::FileId;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::commands::transfer::Dropped;

/// Locks `m`; the guarded values stay consistent even if a holder panicked.
fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The voice note being recorded, if any.
#[derive(Default)]
pub(crate) struct Voice(StdMutex<Recording>);

#[derive(Default)]
struct Recording {
    recorder: Option<Recorder>,
    /// Bumped by every reset: a recorder started before it is not kept.
    epoch: u64,
}

/// Why a started recorder was not kept.
pub(crate) enum NotKept {
    AlreadyRecording(Recorder),
    /// The identity was locked or replaced while the microphone opened.
    Reset(Recorder),
}

impl Voice {
    /// A ticket for [`Self::keep`]; `None` while a note is being recorded.
    pub(crate) fn ticket(&self) -> Option<u64> {
        let recording = lock(&self.0);
        recording.recorder.is_none().then_some(recording.epoch)
    }

    /// Keeps `recorder`, started with `ticket`, unless another one started
    /// or the voice was reset since.
    pub(crate) fn keep(&self, ticket: u64, recorder: Recorder) -> Result<(), NotKept> {
        let mut recording = lock(&self.0);
        if recording.recorder.is_some() {
            return Err(NotKept::AlreadyRecording(recorder));
        }
        if recording.epoch != ticket {
            return Err(NotKept::Reset(recorder));
        }
        recording.recorder = Some(recorder);
        Ok(())
    }

    pub(crate) fn take(&self) -> Option<Recorder> {
        lock(&self.0).recorder.take()
    }

    /// Stops recording and drops what was heard; the microphone is closed
    /// when this returns.
    pub(crate) async fn reset(&self) {
        let recorder = {
            let mut recording = lock(&self.0);
            recording.epoch = recording.epoch.wrapping_add(1);
            recording.recorder.take()
        };
        if let Some(recorder) = recorder {
            let _ = tauri::async_runtime::spawn_blocking(move || recorder.cancel()).await;
        }
    }

    #[cfg(test)]
    pub(crate) fn hold(&self, recorder: Recorder) {
        lock(&self.0).recorder = Some(recorder);
    }

    #[cfg(test)]
    pub(crate) fn is_recording(&self) -> bool {
        lock(&self.0).recorder.is_some()
    }
}

/// Files offered to us, under the name the core sanitized, until they are
/// accepted or their transfer ends; then accepted files, under the name
/// reserved for them, until their transfer ends. Follows the client's
/// events, so it holds no more than the core's open transfers.
#[derive(Clone, Default)]
pub(crate) struct Offers(Arc<StdMutex<OfferBook>>);

#[derive(Default)]
struct OfferBook {
    offered: HashMap<FileId, String>,
    accepted: HashMap<FileId, PathBuf>,
    /// Bumped by every reset: an accept begun before it puts nothing back.
    epoch: u64,
}

impl Offers {
    /// Takes in one of the client's events. Returns the reserved name of an
    /// accepted file whose transfer failed, to release if still empty.
    pub(crate) fn track(&self, event: &Event) -> Option<PathBuf> {
        let mut book = lock(&self.0);
        if let Event::TransferOffered { file_id, name, .. } = event {
            book.offered.insert(*file_id, name.clone());
        } else if let Event::TransferFailed { file_id, .. } = event {
            book.offered.remove(file_id);
            return book.accepted.remove(file_id);
        } else if let Event::TransferComplete { file_id } = event {
            book.offered.remove(file_id);
            book.accepted.remove(file_id);
        }
        None
    }

    /// Takes the offer while it is being accepted, so a second accept finds
    /// none; the ticket is for [`Self::accepted`] and [`Self::restore`].
    pub(crate) fn take(&self, file_id: &FileId) -> Option<(String, u64)> {
        let mut book = lock(&self.0);
        let name = book.offered.remove(file_id)?;
        Some((name, book.epoch))
    }

    /// Notes the name reserved for an accepted file, unless the identity
    /// changed since the offer was taken.
    pub(crate) fn accepted(&self, file_id: FileId, reserved: PathBuf, ticket: u64) {
        let mut book = lock(&self.0);
        if book.epoch == ticket {
            book.accepted.insert(file_id, reserved);
        }
    }

    /// Accepting failed: the offer is open again, unless the identity
    /// changed since it was taken.
    pub(crate) fn restore(&self, file_id: FileId, name: String, ticket: u64) {
        let mut book = lock(&self.0);
        book.accepted.remove(&file_id);
        if book.epoch == ticket {
            book.offered.insert(file_id, name);
        }
    }

    /// The user turned the offer down.
    pub(crate) fn forget(&self, file_id: &FileId) {
        lock(&self.0).offered.remove(file_id);
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, file_id: &FileId) -> bool {
        lock(&self.0).offered.contains_key(file_id)
    }

    fn reset(&self) {
        let mut book = lock(&self.0);
        book.offered.clear();
        book.accepted.clear();
        book.epoch = book.epoch.wrapping_add(1);
    }
}

/// Files offered to us, and the last files dropped on the window.
#[derive(Default)]
pub(crate) struct Transfers {
    pub offers: Offers,
    pub dropped: StdMutex<Dropped>,
}

impl Transfers {
    pub(crate) fn reset(&self) {
        self.offers.reset();
        lock(&self.dropped).forget();
    }
}

/// This new device, waiting to be linked.
#[derive(Default)]
pub(crate) struct Linking(Mutex<LinkWait>);

#[derive(Default)]
struct LinkWait {
    waiting: Option<Provisioning>,
    /// Stays cancelled once cancelled, so a finish that has taken the wait
    /// but not started waiting yet still sees the cancel.
    cancel: CancellationToken,
}

impl Linking {
    /// Waits anew; a finish still waiting on an earlier offer stops.
    pub(crate) async fn start(&self, waiting: Provisioning) {
        let mut wait = self.0.lock().await;
        wait.cancel.cancel();
        *wait = LinkWait {
            waiting: Some(waiting),
            cancel: CancellationToken::new(),
        };
    }

    /// The wait to finish and what cancels it; `None` when not waiting.
    pub(crate) async fn take(&self) -> Option<(Provisioning, CancellationToken)> {
        let mut wait = self.0.lock().await;
        let waiting = wait.waiting.take()?;
        Some((waiting, wait.cancel.clone()))
    }

    /// Stops waiting, also a finish already under way.
    pub(crate) async fn cancel(&self) {
        let mut wait = self.0.lock().await;
        wait.cancel.cancel();
        wait.waiting = None;
    }
}
