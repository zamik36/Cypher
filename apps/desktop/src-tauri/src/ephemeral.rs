//! What belongs to the unlocked identity besides its client, each part
//! owning its state and how it ends: all of it goes when the identity is
//! locked, erased or replaced, so nothing carries over to the next one.

use std::sync::{Mutex as StdMutex, MutexGuard, PoisonError};

use cypher_client::Provisioning;
use cypher_media::Recorder;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

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
