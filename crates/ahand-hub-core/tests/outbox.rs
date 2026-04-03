use ahand_hub_core::Outbox;

#[test]
fn outbox_replays_only_unacked_messages() {
    let mut outbox = Outbox::new(8);
    let seq1 = outbox.store_raw(vec![1]);
    let _seq2 = outbox.store_raw(vec![2]);
    outbox.on_peer_ack(seq1);

    let replay = outbox.replay_from(0);

    assert_eq!(replay, vec![vec![2]]);
}

#[test]
fn outbox_tracks_local_ack_and_trims_buffer() {
    let mut outbox = Outbox::new(2);

    let seq1 = outbox.reserve_seq();
    outbox.store(seq1, vec![1]);
    let seq2 = outbox.store_raw(vec![2]);
    let seq3 = outbox.store_raw(vec![3]);
    outbox.on_recv(4);
    outbox.on_recv(3);

    assert_eq!(seq1, 1);
    assert_eq!(seq2, 2);
    assert_eq!(seq3, 3);
    assert_eq!(outbox.local_ack(), 4);
    assert_eq!(outbox.replay_from(0), vec![vec![2], vec![3]]);
    assert_eq!(outbox.replay_from(2), vec![vec![3]]);
}

#[test]
fn outbox_try_on_peer_ack_rejects_impossible_ack_and_preserves_buffer() {
    let mut outbox = Outbox::new(4);
    let seq = outbox.store_raw(vec![9]);

    assert_eq!(outbox.last_issued_seq(), seq);
    assert!(!outbox.try_on_peer_ack(seq + 1));
    assert_eq!(outbox.replay_from(0), vec![vec![9]]);

    assert!(outbox.try_on_peer_ack(seq));
    assert!(outbox.is_empty());
}

#[test]
fn outbox_remove_drops_specific_buffered_message() {
    let mut outbox = Outbox::new(4);
    let seq1 = outbox.store_raw(vec![1]);
    let seq2 = outbox.store_raw(vec![2]);

    outbox.remove(seq1);

    assert_eq!(outbox.replay_from(0), vec![vec![2]]);

    outbox.remove(seq2);

    assert!(outbox.is_empty());
}

#[test]
fn outbox_remove_missing_sequence_is_a_noop() {
    let mut outbox = Outbox::new(2);
    outbox.store_raw(vec![7]);

    outbox.remove(99);

    assert_eq!(outbox.replay_from(0), vec![vec![7]]);
    assert!(!outbox.is_empty());
}

#[test]
fn outbox_evicts_oldest_when_buffer_exceeds_capacity() {
    let mut outbox = Outbox::new(2);
    outbox.store_raw(vec![1]);
    outbox.store_raw(vec![2]);
    outbox.store_raw(vec![3]);

    let replay = outbox.replay_from(0);

    assert_eq!(replay, vec![vec![2], vec![3]]);
}

#[test]
fn outbox_replay_from_exact_seq_excludes_that_message() {
    let mut outbox = Outbox::new(4);
    let seq1 = outbox.store_raw(vec![10]);
    let _seq2 = outbox.store_raw(vec![20]);

    let replay = outbox.replay_from(seq1);

    assert_eq!(replay, vec![vec![20]]);
}

#[test]
fn outbox_zero_capacity_evicts_every_message_immediately() {
    let mut outbox = Outbox::new(0);
    outbox.store_raw(vec![1]);
    outbox.store_raw(vec![2]);

    assert!(outbox.is_empty());
    assert_eq!(outbox.replay_from(0), Vec::<Vec<u8>>::new());
    assert_eq!(outbox.last_issued_seq(), 2);
}

#[test]
fn outbox_last_issued_seq_is_zero_on_fresh_outbox() {
    let outbox = Outbox::new(4);

    assert_eq!(outbox.last_issued_seq(), 0);
    assert!(outbox.is_empty());
}
