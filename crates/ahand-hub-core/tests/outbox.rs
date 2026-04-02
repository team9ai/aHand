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
