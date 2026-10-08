//! A new device waiting to be linked: drives a [`Provision`] over a gateway
//! connection until a device of the identity hands it over.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_core::link::{Linked, Provision};
use cypher_core::{Effect, Event, Input};
use rand::rngs::OsRng;
use tokio::sync::{mpsc, oneshot};

use crate::ClientError;
use crate::driver::now_ms;
use crate::net::{self, Link, NetEvent};

const TICK: Duration = Duration::from_secs(1);
const RECONNECT_AFTER: Duration = Duration::from_secs(2);

/// A new device showing its offer; dropping it stops waiting.
pub struct Provisioning {
    offer: String,
    done: oneshot::Receiver<Result<Linked, ClientError>>,
}

impl Provisioning {
    /// What to show for a device of the identity to scan.
    pub fn offer(&self) -> &str {
        &self.offer
    }

    /// The identity, once a device of it handed it over; [`ClientError::Expired`]
    /// when nobody did in time.
    pub async fn linked(self) -> Result<Linked, ClientError> {
        self.done.await.map_err(|_| ClientError::Closed)?
    }
}

/// Starts waiting, as a device called `name`, on the gateway at `addr`.
pub fn provision(addr: String, tls: Arc<rustls::ClientConfig>, name: &str) -> Provisioning {
    let machine = Provision::new(name, now_ms(), &mut OsRng);
    let offer = machine.offer().to_text();
    let (done_tx, done) = oneshot::channel();
    tokio::spawn(run(machine, addr, tls, done_tx));
    Provisioning { offer, done }
}

async fn run(
    mut machine: Provision,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    mut done: oneshot::Sender<Result<Linked, ClientError>>,
) {
    let (events_tx, mut events) = mpsc::unbounded_channel();
    net::spawn(
        Link::Gateway,
        addr.clone(),
        Arc::clone(&tls),
        events_tx.clone(),
    );
    let mut out: Option<mpsc::Sender<Bytes>> = None;
    let mut tick = tokio::time::interval(TICK);
    loop {
        let input = tokio::select! {
            () = done.closed() => return,
            _ = tick.tick() => Input::Tick,
            event = events.recv() => match event {
                Some(NetEvent::Up(_, sender)) => {
                    out = Some(sender);
                    Input::Connected
                }
                Some(NetEvent::Frame(_, frame)) => Input::Frame(frame),
                Some(NetEvent::Down(_)) => {
                    out = None;
                    tokio::time::sleep(RECONNECT_AFTER).await;
                    net::spawn(Link::Gateway, addr.clone(), Arc::clone(&tls), events_tx.clone());
                    Input::Disconnected
                }
                None => return,
            },
        };
        for effect in machine.handle(input, now_ms()) {
            match effect {
                Effect::Transmit(frame) => {
                    if let Some(out) = &out {
                        let _ = out.try_send(frame);
                    }
                }
                Effect::Emit(Event::LinkedHere) => {
                    let linked = machine.take_linked().ok_or(ClientError::Closed);
                    let _ = done.send(linked);
                    return;
                }
                Effect::Emit(Event::LinkFailed { .. }) => {
                    let _ = done.send(Err(ClientError::Expired));
                    return;
                }
                _ => {}
            }
        }
    }
}
