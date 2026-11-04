use std::sync::Arc;

use bytes::Bytes;
use cypher_server_kit::{Origin, SEALED_REFUSED, SIG_REQUEST_SUBJECT, peer_subject};
use cypher_types::Addr;
use cypher_wire::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, SendView, ServerMsg, access_hash, encode_recv,
    encode_recv_sealed, peek_send,
};
use tokio::time::Instant;
use tracing::debug;

use crate::bus::{Bus, BusError, BusMsg};

use super::{
    LIMIT_NOTICE_INTERVAL, PEER_REQUEST_TIMEOUT, SIG_REQUEST_TIMEOUT, STATUS_BUSY,
    STATUS_DELIVERED, Session, error, frame,
};

impl<B: Bus> Session<B> {
    /// Returns false when the connection must be closed.
    pub(super) async fn on_frame(&mut self, me: Addr, bytes: Bytes) -> bool {
        self.gw.metrics.frames_in.inc();
        self.gw.metrics.bytes_in.inc_by(bytes.len() as u64);
        if !self.limiter.admit(bytes.len()) {
            self.gw.metrics.rate_limited.inc();
            self.notice_rate_limited(&bytes);
            return true;
        }
        if let Some(send) = peek_send(&bytes) {
            self.route(me, send).await;
            return true;
        }
        let Ok(Frame { req_id, msg }) = Frame::<ClientMsg>::decode(bytes.clone()) else {
            return false;
        };
        match msg {
            ClientMsg::Ping => {
                self.out.try_push(frame(req_id, ServerMsg::Pong));
                true
            }
            ClientMsg::SetAccess { hashes } => {
                self.access = hashes;
                self.out.try_push(frame(req_id, ServerMsg::Done));
                true
            }
            ClientMsg::Hello { .. } | ClientMsg::Auth { .. } | ClientMsg::Send { .. } => false,
            _ => {
                self.forward_to_signaling(me, req_id, bytes);
                true
            }
        }
    }

    /// Answers a frame dropped by the rate limiter with `RateLimited`, so
    /// the client need not wait for its request to time out. At most once
    /// per `LIMIT_NOTICE_INTERVAL`: answering every dropped frame would let
    /// a flood make the gateway write as much as it reads.
    fn notice_rate_limited(&mut self, dropped: &Bytes) {
        let now = Instant::now();
        if now < self.next_limit_notice {
            return;
        }
        let req_id = match peek_send(dropped) {
            Some(send) => send.req_id,
            None => match Frame::<ClientMsg>::decode(dropped.clone()) {
                Ok(frame) => frame.req_id,
                Err(_) => return,
            },
        };
        self.next_limit_notice = now + LIMIT_NOTICE_INTERVAL;
        self.out
            .try_push(frame(req_id, error(ErrorCode::RateLimited)));
    }

    /// Relays a `Send` to the device it names; a device may write to its
    /// siblings, never to itself.
    async fn route(&self, me: Addr, send: SendView) {
        let SendView {
            req_id,
            to,
            device,
            want_ack,
            body,
        } = send;
        let to = Addr::new(to, device);
        if to == me {
            return;
        }
        let relayed = encode_recv(&me.peer, me.device, &body);
        if let Some(target) = self.gw.registry.get(&to) {
            let ok = target.outbox.try_push(relayed);
            if ok {
                self.gw.metrics.delivered_local.inc();
            } else {
                self.gw.metrics.busy.inc();
            }
            if want_ack {
                let status = if ok {
                    DeliveryStatus::Delivered
                } else {
                    DeliveryStatus::Busy
                };
                self.ack(req_id, status);
            }
            return;
        }
        let subject = peer_subject(&to.peer.to_hex(), to.device, Origin::Gateway);
        if !want_ack {
            self.publish(subject, relayed).await;
            return;
        }
        let Ok(permit) = Arc::clone(&self.ack_permits).try_acquire_owned() else {
            self.gw.metrics.busy.inc();
            self.ack(req_id, DeliveryStatus::Busy);
            return;
        };
        let gw = Arc::clone(&self.gw);
        let out = self.out.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let status = match gw
                .bus
                .request(subject, None, relayed, PEER_REQUEST_TIMEOUT)
                .await
            {
                Ok(reply) if reply.first() == Some(&STATUS_DELIVERED) => {
                    gw.metrics.delivered_remote.inc();
                    DeliveryStatus::Delivered
                }
                Ok(_) | Err(BusError::Unavailable) => DeliveryStatus::Busy,
                Err(BusError::NoResponders | BusError::Timeout) => DeliveryStatus::Offline,
            };
            out.try_push(frame(req_id, ServerMsg::SendAck { status }));
        });
    }

    fn ack(&self, req_id: u32, status: DeliveryStatus) {
        self.out
            .try_push(frame(req_id, ServerMsg::SendAck { status }));
    }

    /// A frame another node relayed to this peer.
    pub(super) async fn on_relayed(&self, msg: BusMsg) {
        let ok = self.out.try_push(msg.payload);
        if !ok {
            self.gw.metrics.busy.inc();
        }
        if let Some(reply) = msg.reply {
            let status = if ok { STATUS_DELIVERED } else { STATUS_BUSY };
            self.publish(reply, Bytes::copy_from_slice(&[status])).await;
        }
    }

    /// A delivery signaling relays without a sender (`token ‖ item`): the
    /// device gets it only with a token it told this gateway, so signaling
    /// cannot write to devices that did not hand their contacts one.
    pub(super) async fn on_sealed(&self, msg: BusMsg) {
        let status = match msg.payload.split_first_chunk::<32>() {
            Some((token, item)) if knows(&self.access, &access_hash(token)) => {
                if self.out.try_push(encode_recv_sealed(item)) {
                    self.gw.metrics.sealed_delivered.inc();
                    STATUS_DELIVERED
                } else {
                    self.gw.metrics.busy.inc();
                    STATUS_BUSY
                }
            }
            _ => {
                self.gw.metrics.sealed_refused.inc();
                SEALED_REFUSED
            }
        };
        if let Some(reply) = msg.reply {
            self.publish(reply, Bytes::copy_from_slice(&[status])).await;
        }
    }

    /// Publishes on the bus, where nothing waits for an answer: a failure
    /// is only counted. False when the bus did not take it.
    async fn publish(&self, subject: String, payload: Bytes) -> bool {
        let Err(error) = self.gw.bus.publish(subject, payload).await else {
            return true;
        };
        self.gw.metrics.publish_failed.inc();
        debug!(?error, "bus publish failed");
        false
    }

    fn forward_to_signaling(&self, me: Addr, req_id: u32, request: Bytes) {
        let Ok(permit) = Arc::clone(&self.sig_permits).try_acquire_owned() else {
            self.out
                .try_push(frame(req_id, error(ErrorCode::RateLimited)));
            return;
        };
        let gw = Arc::clone(&self.gw);
        let out = self.out.clone();
        tokio::spawn(async move {
            let reply = gw
                .bus
                .request(
                    SIG_REQUEST_SUBJECT.to_owned(),
                    Some(me.peer),
                    request,
                    SIG_REQUEST_TIMEOUT,
                )
                .await
                .unwrap_or_else(|_| frame(req_id, error(ErrorCode::Unavailable)));
            out.try_push(reply);
            drop(permit);
        });
    }
}

/// Whether `hash` is one of `known`, comparing every byte of each.
fn knows(known: &[[u8; 32]], hash: &[u8; 32]) -> bool {
    let matched = known.iter().fold(0u8, |matched, k| {
        let diff = k.iter().zip(hash).fold(0u8, |acc, (a, b)| acc | (a ^ b));
        matched | u8::from(diff == 0)
    });
    matched != 0
}
