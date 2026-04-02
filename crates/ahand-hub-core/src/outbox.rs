use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Outbox {
    next_seq: u64,
    peer_ack: u64,
    local_ack: u64,
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

    pub fn reserve_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    pub fn store(&mut self, seq: u64, data: Vec<u8>) {
        self.buffer.push_back((seq, data));
        while self.buffer.len() > self.max_buffer {
            self.buffer.pop_front();
        }
    }

    pub fn store_raw(&mut self, data: Vec<u8>) -> u64 {
        let seq = self.reserve_seq();
        self.store(seq, data);
        seq
    }

    pub fn on_recv(&mut self, seq: u64) {
        self.local_ack = self.local_ack.max(seq);
    }

    pub fn on_peer_ack(&mut self, ack: u64) {
        self.peer_ack = self.peer_ack.max(ack);
        while let Some((seq, _)) = self.buffer.front() {
            if *seq <= self.peer_ack {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn try_on_peer_ack(&mut self, ack: u64) -> bool {
        if ack > self.last_issued_seq() {
            return false;
        }
        self.on_peer_ack(ack);
        true
    }

    pub fn replay_from(&self, last_ack: u64) -> Vec<Vec<u8>> {
        self.buffer
            .iter()
            .filter(|(seq, _)| *seq > last_ack)
            .map(|(_, data)| data.clone())
            .collect()
    }

    pub fn local_ack(&self) -> u64 {
        self.local_ack
    }

    pub fn last_issued_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    pub fn remove(&mut self, seq: u64) {
        if let Some(index) = self
            .buffer
            .iter()
            .position(|(buffered_seq, _)| *buffered_seq == seq)
        {
            self.buffer.remove(index);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}
