CREATE TABLE reward_event (
    guild_id TEXT NOT NULL,
    event_key TEXT NOT NULL,
    user_id TEXT NOT NULL,
    reason TEXT NOT NULL REFERENCES transaction_reason(code)
        CHECK (reason IN ('MESSAGE', 'REACTION', 'REACTION_RECEIVED', 'REPLY_RECEIVED')),
    received_at INTEGER NOT NULL CHECK (typeof(received_at) = 'integer' AND received_at >= 0),
    entry_id INTEGER UNIQUE REFERENCES not_transaction(id),
    PRIMARY KEY (guild_id, event_key)
);

CREATE INDEX reward_event_limits_idx
    ON reward_event (guild_id, user_id, reason, received_at)
    WHERE entry_id IS NOT NULL;
