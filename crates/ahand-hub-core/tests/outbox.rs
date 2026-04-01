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
