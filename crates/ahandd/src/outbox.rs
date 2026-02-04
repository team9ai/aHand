use std::collections::VecDeque;

use ahand_protocol::Envelope;
use prost::Message;

/// Outbox tracks outbound seq, inbound ack, and buffers unacknowledged messages
/// for replay on reconnect.
pub struct Outbox {
    next_seq: u64,
    /// Highest seq the peer has acknowledged.
    peer_ack: u64,
    /// Highest seq we have received from the peer.
    local_ack: u64,
    /// Buffer of (seq, encoded bytes) for unacked outbound messages.
    buffer: VecDeque<(u64, Vec<u8>)>,
    max_buffer: usize,
}

impl Outbox {
    pub fn new(max_buffer: usize) -> Self {
        Self {
            next_seq: 1,
            peer_ack: 0,
            local_ack: 0,
            buffer: VecDeque::new(),
            max_buffer,
        }
    }

    /// Assign the next seq and current local_ack to an outbound envelope.
    /// Returns the assigned seq.
    pub fn stamp(&mut self, envelope: &mut Envelope) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        envelope.seq = seq;
        envelope.ack = self.local_ack;
        seq
    }

    /// Store an encoded message in the outbox buffer for potential replay.
    pub fn store(&mut self, seq: u64, data: Vec<u8>) {
        self.buffer.push_back((seq, data));
        // Evict oldest if over capacity.
        while self.buffer.len() > self.max_buffer {
            self.buffer.pop_front();
        }
    }

    /// Called when we receive a message from the peer — update local_ack.
    pub fn on_recv(&mut self, seq: u64) {
        if seq > self.local_ack {
            self.local_ack = seq;
        }
    }

    /// Called when we see the peer's ack field — remove acknowledged messages.
    pub fn on_peer_ack(&mut self, ack: u64) {
        if ack > self.peer_ack {
            self.peer_ack = ack;
        }
        while let Some((seq, _)) = self.buffer.front() {
            if *seq <= self.peer_ack {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }

    /// After reconnect, drain all unacked messages for replay.
    pub fn drain_unacked(&self) -> Vec<Vec<u8>> {
        self.buffer.iter().map(|(_, data)| data.clone()).collect()
    }

    /// The highest seq we received from the peer, used in Hello.last_ack on reconnect.
    pub fn local_ack(&self) -> u64 {
        self.local_ack
    }

    /// Number of buffered (unacked) messages.
    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.buffer.len()
    }
}

/// Stamp, encode, store in outbox, and return the encoded bytes.
pub fn prepare_outbound(outbox: &mut Outbox, envelope: &mut Envelope) -> Vec<u8> {
    let seq = outbox.stamp(envelope);
    let data = envelope.encode_to_vec();
    outbox.store(seq, data.clone());
    data
}
