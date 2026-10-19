-- The caller owns the transaction and enables foreign keys on every connection.
CREATE TABLE transaction_reason (
    code TEXT PRIMARY KEY NOT NULL
);

CREATE TABLE transaction_reason_revision (
    code TEXT NOT NULL REFERENCES transaction_reason(code),
    revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (code, revision)
);

CREATE TABLE content_file (
    key TEXT PRIMARY KEY NOT NULL,
    filename TEXT NOT NULL CHECK (length(filename) > 0),
    size_bytes INTEGER NOT NULL CHECK (typeof(size_bytes) = 'integer' AND size_bytes > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE shop_item (
    id INTEGER PRIMARY KEY,
    guild_id TEXT NOT NULL,
    seller_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('ROLE', 'CONTENT')),
    target TEXT NOT NULL,
    UNIQUE (guild_id, kind, target)
);

CREATE TABLE shop_item_revision (
    item_id INTEGER NOT NULL REFERENCES shop_item(id),
    revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    description TEXT NOT NULL DEFAULT '',
    price INTEGER NOT NULL CHECK (typeof(price) = 'integer' AND price > 0),
    deleted_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (item_id, revision)
);

CREATE TABLE shop_item_publication (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL,
    item_revision INTEGER NOT NULL,
    channel_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (item_id, item_revision)
        REFERENCES shop_item_revision(item_id, revision),
    UNIQUE (item_id, item_revision, channel_id, message_id)
);

CREATE INDEX shop_item_publication_item_idx
    ON shop_item_publication (item_id, id);

CREATE TABLE not_transaction (
    id INTEGER PRIMARY KEY,
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    amount INTEGER NOT NULL CHECK (typeof(amount) = 'integer'),
    reason TEXT NOT NULL REFERENCES transaction_reason(code),
    event_key TEXT NOT NULL,
    item_id INTEGER REFERENCES shop_item(id),
    sale_of INTEGER UNIQUE REFERENCES not_transaction(id),
    refund_of INTEGER UNIQUE REFERENCES not_transaction(id),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (guild_id, event_key),
    CHECK (
        (reason = 'PURCHASE' AND amount < 0 AND item_id IS NOT NULL
            AND sale_of IS NULL AND refund_of IS NULL)
        OR (reason = 'SALE' AND amount > 0 AND item_id IS NULL
            AND sale_of IS NOT NULL AND refund_of IS NULL)
        OR (reason = 'REFUND' AND amount > 0 AND item_id IS NULL
            AND sale_of IS NULL AND refund_of IS NOT NULL)
        OR (reason = 'SALE_REFUND' AND amount < 0 AND item_id IS NULL
            AND sale_of IS NULL AND refund_of IS NOT NULL)
        OR (reason IN ('MESSAGE', 'REACTION', 'REACTION_RECEIVED',
                      'REPLY_RECEIVED', 'TIP_RECEIVED') AND amount > 0
            AND item_id IS NULL AND sale_of IS NULL AND refund_of IS NULL)
        OR (reason = 'TIP_SENT' AND amount < 0
            AND item_id IS NULL AND sale_of IS NULL AND refund_of IS NULL)
        OR (reason = 'TIMEOUT' AND amount <= 0
            AND item_id IS NULL AND sale_of IS NULL AND refund_of IS NULL)
        OR (reason = 'ADJUSTMENT'
            AND item_id IS NULL AND sale_of IS NULL AND refund_of IS NULL)
    )
);

CREATE INDEX not_transaction_balance_idx
    ON not_transaction (guild_id, user_id);

INSERT INTO transaction_reason (code) VALUES
    ('MESSAGE'), ('REACTION'), ('REACTION_RECEIVED'), ('REPLY_RECEIVED'),
    ('TIP_SENT'), ('TIP_RECEIVED'), ('TIMEOUT'), ('PURCHASE'), ('SALE'),
    ('REFUND'), ('SALE_REFUND'), ('ADJUSTMENT');

INSERT INTO transaction_reason_revision (code, revision, name) VALUES
    ('MESSAGE', 1, 'メッセージ投稿'),
    ('REACTION', 1, 'リアクションする'),
    ('REACTION_RECEIVED', 1, 'リアクションされる'),
    ('REPLY_RECEIVED', 1, '返信される'),
    ('TIP_SENT', 1, 'NOT譲渡'),
    ('TIP_RECEIVED', 1, 'NOT受取'),
    ('TIMEOUT', 1, 'ルール違反による減算'),
    ('PURCHASE', 1, '商品購入'),
    ('SALE', 1, '商品売上'),
    ('REFUND', 1, '購入者への返金'),
    ('SALE_REFUND', 1, '出品者の売上取消'),
    ('ADJUSTMENT', 1, '管理者による残高調整');

CREATE VIEW current_transaction_reason (code, revision, name) AS
SELECT r.code, r.revision, r.name
FROM transaction_reason_revision AS r
WHERE r.revision = (
    SELECT MAX(h.revision) FROM transaction_reason_revision AS h
    WHERE h.code = r.code
);

CREATE VIEW current_shop_item
    (id, guild_id, seller_id, kind, target, revision, name, description, price, deleted_at) AS
SELECT i.id, i.guild_id, i.seller_id, i.kind, i.target,
       r.revision, r.name, r.description, r.price, r.deleted_at
FROM shop_item AS i
JOIN shop_item_revision AS r ON r.item_id = i.id
WHERE r.revision = (
    SELECT MAX(h.revision) FROM shop_item_revision AS h
    WHERE h.item_id = i.id
);

CREATE VIEW not_balance (guild_id, user_id, balance) AS
SELECT guild_id, user_id, SUM(amount)
FROM not_transaction
GROUP BY guild_id, user_id;
