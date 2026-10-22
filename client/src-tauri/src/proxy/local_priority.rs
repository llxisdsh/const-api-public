// All local selectors share tier and load semantics, after their own model,
// capability, protocol and ownership filters. No marketplace weights change.
fn local_channel_priority(cfg: &ClientConfig, channel: &ChannelConfig) -> u32 {
    if let Some(priority) = channel.v2.local_priority {
        return priority;
    }
    // Raw legacy configurations can also reach selectors in tests / previews.
    let mut last = cfg.channels.iter().filter_map(|item| item.v2.local_priority).max().unwrap_or(0);
    for item in &cfg.channels {
        if item.v2.local_priority.is_none() {
            last = last.saturating_add(1);
        }
        if item.id == channel.id {
            return last;
        }
    }
    unreachable!("local routing candidate must belong to the current config")
}

fn local_priority_candidates<'a>(
    cfg: &ClientConfig,
    candidates: impl Iterator<Item = (&'a ChannelConfig, u8, u8)>,
) -> Vec<&'a ChannelConfig> {
    let mut best = None;
    let mut tied = Vec::new();
    for (channel, evidence_rank, protocol_rank) in candidates {
        let load = crate::channel_concurrency::current(&channel.id);
        let limit = channel.max_concurrency();
        let rank = (
            evidence_rank,
            protocol_rank,
            // A full channel cannot admit the request: every tier with headroom goes first.
            limit > 0 && load >= limit,
            local_channel_priority(cfg, channel),
            load,
        );
        if best.is_none_or(|previous| rank < previous) {
            best = Some(rank);
            tied.clear();
        }
        if best == Some(rank) {
            tied.push(channel);
        }
    }
    tied
}

fn select_local_priority_channel<'a>(
    cfg: &ClientConfig,
    candidates: impl Iterator<Item = (&'a ChannelConfig, u8, u8)>,
) -> Option<&'a ChannelConfig> {
    let tied = local_priority_candidates(cfg, candidates);
    if tied.is_empty() {
        return None;
    }
    // Random ties avoid an idle first account monopolizing sequential sessions,
    // without per-model cursors or another unbounded process-global state map.
    Some(tied[rand::random_range(0..tied.len())])
}
