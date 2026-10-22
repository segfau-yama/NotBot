use crate::domain;
use crate::domain::StoredContent;
use crate::infrastructure::discord::shop::ShopRepository as DiscordShopRepository;
use crate::infrastructure::{MoneyError, ValidationError};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    domain::{
        Actor, Balance, GuildId, ItemDetails, ItemTarget, LedgerReason, NewLedgerEntry,
        PurchaseResult, ShopItem, UserId,
    },
    infrastructure::{
        MAX_CONTENT_BYTES, RepositoryError,
        discord::content::ContentStorageError,
        sqlite::{DatabaseError, SqliteDatabaseAccess, not::NotRepository, shop::ShopRepository},
    },
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};
use tokio::{sync::oneshot, time::timeout};

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "notbot-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn path(&self) -> PathBuf {
        self.0.join("nested").join("notbot ?# database.sqlite")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn guild() -> GuildId {
    domain::GuildId::new(100).unwrap()
}

fn user() -> UserId {
    domain::UserId::new(200).unwrap()
}

fn entry(amount: i64, key: &str) -> NewLedgerEntry {
    domain::NewLedgerEntry::new(
        guild(),
        user(),
        domain::NotAmount::new(amount),
        LedgerReason::Adjustment,
        key,
    )
    .unwrap()
}

async fn balance(db: &SqliteDatabaseAccess) -> i64 {
    let mut session = db.begin_read().await.unwrap();
    let value = session.get_balance(guild(), user()).await.unwrap().get();
    session.commit().await.unwrap();
    value
}

// スキーマ制約・破損データの検査用。業務処理は上の操作メソッドを呼ぶ。
async fn inspect(path: &Path) -> DatabaseConnection {
    let path = path.to_owned();
    let mut options = ConnectOptions::new("sqlite://notbot.sqlite");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| options.filename(&path).foreign_keys(true));
    Database::connect(options).await.unwrap()
}

async fn number(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap()
}

#[tokio::test]
async fn 同時起動や再起動でも初期値を重複作成せず残高を保持する() {
    let file = TestDatabase::new();
    let path = file.path();
    let db = SqliteDatabaseAccess::open(&path).await.unwrap();
    let raw = inspect(&path).await;
    assert_eq!(number(&raw, "PRAGMA user_version").await, 2);
    assert_eq!(
        number(
            &raw,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        )
        .await,
        8,
    );
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM sqlite_schema WHERE type='view'").await,
        3
    );
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM transaction_reason").await,
        12
    );
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM transaction_reason_revision").await,
        12
    );
    assert_eq!(balance(&db).await, 0);
    let mut write = db.begin_write().await.unwrap();
    write.append_entry(entry(41, "seed")).await.unwrap();
    write.commit().await.unwrap();
    raw.close().await.unwrap();
    db.close().await.unwrap();

    let (first, second) = tokio::join!(
        SqliteDatabaseAccess::open(&path),
        SqliteDatabaseAccess::open(&path),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(balance(&first).await, 41);
    assert_eq!(balance(&second).await, 41);
    let raw = inspect(&path).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM transaction_reason_revision").await,
        12
    );
    raw.close().await.unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn 初期化に失敗するとテーブルや適用済みの版を残さない() {
    let file = TestDatabase::new();
    let path = file.path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // 初期DDLの途中で衝突する既存スキーマを用意する。
    let mut options = ConnectOptions::new("sqlite://unused?mode=rwc");
    let filename = path.clone();
    options.map_sqlx_sqlite_opts(move |options| options.filename(&filename));
    let raw = Database::connect(options).await.unwrap();
    raw.execute_unprepared("CREATE TABLE shop_item (id INTEGER)")
        .await
        .unwrap();
    assert_eq!(
        SqliteDatabaseAccess::open(&path).await.err(),
        Some(DatabaseError::InvalidData)
    );
    assert_eq!(number(&raw, "PRAGMA user_version").await, 0);
    assert_eq!(
        number(
            &raw,
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='transaction_reason'"
        )
        .await,
        0
    );
    assert_eq!(
        number(
            &raw,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'"
        )
        .await,
        1
    );
    raw.close().await.unwrap();
}

#[tokio::test]
async fn 取引内の残高は未確定の増減を含みサーバーと利用者ごとに分かれる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let mut write = db.begin_write().await.unwrap();
    let inserted = write.append_entry(entry(100, "first")).await.unwrap();
    assert_eq!(inserted.amount().get(), 100);
    assert_eq!(inserted.event_key(), "first");
    write.append_entry(entry(-30, "second")).await.unwrap();
    assert_eq!(write.get_balance(guild(), user()).await.unwrap().get(), 70);
    let other_guild = domain::GuildId::new(101).unwrap();
    let other_user = domain::UserId::new(201).unwrap();
    for (guild, user, key) in [
        (other_guild, user(), "first"),
        (guild(), other_user, "other-user"),
    ] {
        write
            .append_entry(
                domain::NewLedgerEntry::new(
                    guild,
                    user,
                    domain::NotAmount::new(900),
                    LedgerReason::Adjustment,
                    key,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(write.get_balance(guild, user).await.unwrap().get(), 900);
    }
    // 別接続には確定前の追記を見せない。
    assert_eq!(balance(&db).await, 0);
    write.commit().await.unwrap();
    assert_eq!(balance(&db).await, 70);
    db.close().await.unwrap();
}

#[tokio::test]
async fn 読取専用の操作と取消や途中失敗では確定済みの履歴を変えない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let mut seed = db.begin_write().await.unwrap();
    seed.append_entry(entry(100, "seed")).await.unwrap();
    seed.commit().await.unwrap();

    let mut read = db.begin_read().await.unwrap();
    assert_eq!(
        read.append_entry(entry(1, "denied")).await,
        Err(DatabaseError::ReadOnly)
    );
    assert_eq!(read.get_balance(guild(), user()).await.unwrap().get(), 100);
    read.commit().await.unwrap();

    let mut write = db.begin_write().await.unwrap();
    write.append_entry(entry(40, "rolled-back")).await.unwrap();
    write.rollback().await.unwrap();
    let mut write = db.begin_write().await.unwrap();
    write.append_entry(entry(40, "dropped")).await.unwrap();
    drop(write);
    let mut write = db.begin_write().await.unwrap();
    assert_eq!(write.get_balance(guild(), user()).await.unwrap().get(), 100);
    write
        .append_entry(entry(40, "before-failure"))
        .await
        .unwrap();
    assert_eq!(
        write.append_entry(entry(1, "seed")).await,
        Err(DatabaseError::Conflict)
    );
    assert_eq!(write.commit().await, Err(DatabaseError::Conflict));
    assert_eq!(balance(&db).await, 100);
    let raw = inspect(&file.path()).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        1
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn どの接続からも存在しない参照先を持つ履歴を登録できない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let missing_sale = || {
        domain::NewLedgerEntry::new(
            guild(),
            user(),
            domain::NotAmount::new(1),
            LedgerReason::Sale(domain::LedgerEntryId::new(999).unwrap()),
            "missing-purchase",
        )
        .unwrap()
    };
    // 片方を借りたままにして、もう片方を確実に検証する。
    let first = db.begin_read().await.unwrap();
    let mut second = db.begin_write().await.unwrap();
    second
        .append_entry(entry(10, "before-fk-failure"))
        .await
        .unwrap();
    assert_eq!(
        second.append_entry(missing_sale()).await,
        Err(DatabaseError::InvalidData)
    );
    assert_eq!(second.commit().await, Err(DatabaseError::InvalidData));
    let second = db.begin_read().await.unwrap();
    first.rollback().await.unwrap();
    let mut first = db.begin_write().await.unwrap();
    assert_eq!(
        first.append_entry(missing_sale()).await,
        Err(DatabaseError::InvalidData)
    );
    assert_eq!(first.commit().await, Err(DatabaseError::InvalidData));
    second.rollback().await.unwrap();
    assert_eq!(balance(&db).await, 0);
    db.close().await.unwrap();
}

#[tokio::test]
async fn 負の集計残高や整数上限を超える集計は取引全体を失敗させる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let mut write = db.begin_write().await.unwrap();
    write.append_entry(entry(-1, "negative")).await.unwrap();
    assert_eq!(
        write.get_balance(guild(), user()).await,
        Err(DatabaseError::InvalidData)
    );
    assert_eq!(write.commit().await, Err(DatabaseError::InvalidData));
    assert_eq!(balance(&db).await, 0);

    let mut write = db.begin_write().await.unwrap();
    write.append_entry(entry(i64::MAX, "max")).await.unwrap();
    write.append_entry(entry(1, "overflow")).await.unwrap();
    assert_eq!(
        write.get_balance(guild(), user()).await,
        Err(DatabaseError::InvalidData)
    );
    assert_eq!(write.commit().await, Err(DatabaseError::InvalidData));
    assert_eq!(balance(&db).await, 0);
    db.close().await.unwrap();
}

#[tokio::test]
async fn 同時の残高変更は先の確定を待ち待機時間を超えたら競合として返す() {
    let file = TestDatabase::new();
    let first = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let second = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let mut seed = first.begin_write().await.unwrap();
    seed.append_entry(entry(100, "seed")).await.unwrap();
    seed.commit().await.unwrap();
    let mut write = first.begin_write().await.unwrap();
    assert_eq!(write.get_balance(guild(), user()).await.unwrap().get(), 100);
    write.append_entry(entry(-80, "debit")).await.unwrap();
    let competitor = second.clone();
    let mut waiting = tokio::spawn(async move {
        let mut transaction = competitor.begin_write().await.unwrap();
        let value = transaction
            .get_balance(guild(), user())
            .await
            .unwrap()
            .get();
        transaction.rollback().await.unwrap();
        value
    });
    assert!(
        timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    write.commit().await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap(),
        20
    );

    let held = first.begin_write().await.unwrap();
    assert_eq!(
        second.begin_write().await.err(),
        Some(DatabaseError::Conflict)
    );
    held.rollback().await.unwrap();
    second
        .begin_write()
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn 処理や取引開始を中断しても次の操作に未完了の取引を持ち越さない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let task_db = db.clone();
    let (ready_tx, ready_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut write = task_db.begin_write().await.unwrap();
        write.append_entry(entry(999, "cancelled")).await.unwrap();
        ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
        write.commit().await.unwrap();
    });
    ready_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let mut write = db.begin_write().await.unwrap();
    assert_eq!(write.get_balance(guild(), user()).await.unwrap().get(), 0);
    write.rollback().await.unwrap();

    let second = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let mut held = db.begin_write().await.unwrap();
    held.append_entry(entry(7, "after-cancel")).await.unwrap();
    assert!(
        timeout(Duration::from_millis(100), second.begin_write())
            .await
            .is_err()
    );
    held.commit().await.unwrap();
    let mut next = timeout(Duration::from_secs(3), second.begin_write())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.get_balance(guild(), user()).await.unwrap().get(), 7);
    next.rollback().await.unwrap();
    db.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn 確定時に制約違反が起きたら追加した全履歴を取り消す() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let raw = inspect(&file.path()).await;
    // 本番コードへ障害注入用の分岐を足さず、SQLite自身にCOMMITを拒否させる。
    raw.execute_unprepared(
        "CREATE TABLE commit_guard (
            code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED
         );
         CREATE TRIGGER reject_ledger_commit AFTER INSERT ON not_transaction
         BEGIN INSERT INTO commit_guard VALUES ('UNKNOWN'); END;",
    )
    .await
    .unwrap();
    let mut write = db.begin_write().await.unwrap();
    write
        .append_entry(entry(100, "must-rollback"))
        .await
        .unwrap();
    assert_eq!(write.commit().await, Err(DatabaseError::InvalidData));
    assert_eq!(balance(&db).await, 0);
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM commit_guard").await, 0);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 不正な履歴を制約で拒否し現在状態には最新の版だけを表示する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let raw = inspect(&file.path()).await;
    raw.execute_unprepared(
        "INSERT INTO content_file (key, filename, size_bytes) VALUES ('file', '資料.pdf', 100);
         INSERT INTO shop_item (id, guild_id, seller_id, kind, target)
             VALUES (1, '100', '200', 'CONTENT', 'file');
         INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (1, 1, 'old', 100);
         INSERT INTO shop_item_publication (item_id, item_revision, channel_id, message_id)
             VALUES (1, 1, '300', '400');
         INSERT INTO not_transaction (id, guild_id, user_id, amount, reason, event_key, item_id)
             VALUES (1, '100', '201', -100, 'PURCHASE', 'purchase', 1);
         INSERT INTO not_transaction (id, guild_id, user_id, amount, reason, event_key, sale_of)
             VALUES (2, '100', '200', 100, 'SALE', 'sale', 1);
         INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key, refund_of)
             VALUES ('100', '201', 100, 'REFUND', 'refund', 1);
         INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key, refund_of)
             VALUES ('100', '200', -100, 'SALE_REFUND', 'sale-refund', 2);
         INSERT INTO transaction_reason_revision (code, revision, name) VALUES ('MESSAGE', 2, 'new');
         INSERT INTO shop_item_revision (item_id, revision, name, price, deleted_at)
             VALUES (1, 2, 'new', 200, CURRENT_TIMESTAMP);",
    ).await.unwrap();
    let mut read = db.begin_read().await.unwrap();
    for (key, reason) in [
        (
            "purchase",
            LedgerReason::Purchase(domain::ItemId::new(1).unwrap()),
        ),
        (
            "sale",
            LedgerReason::Sale(domain::LedgerEntryId::new(1).unwrap()),
        ),
        (
            "refund",
            LedgerReason::Refund(domain::LedgerEntryId::new(1).unwrap()),
        ),
        (
            "sale-refund",
            LedgerReason::SaleRefund(domain::LedgerEntryId::new(2).unwrap()),
        ),
    ] {
        assert_eq!(
            read.find_by_event_key(guild(), key)
                .await
                .unwrap()
                .unwrap()
                .reason(),
            reason
        );
    }
    read.commit().await.unwrap();
    for invalid in [
        "INSERT INTO transaction_reason_revision VALUES ('UNKNOWN', 1, 'bad', CURRENT_TIMESTAMP)",
        "INSERT INTO transaction_reason_revision VALUES ('MESSAGE', 0, 'bad', CURRENT_TIMESTAMP)",
        "INSERT INTO transaction_reason_revision VALUES ('MESSAGE', 1.5, 'bad', CURRENT_TIMESTAMP)",
        "INSERT INTO transaction_reason_revision VALUES ('MESSAGE', 2, 'duplicate', CURRENT_TIMESTAMP)",
        "INSERT INTO content_file (key, filename, size_bytes) VALUES ('bad', 'file', 0)",
        "INSERT INTO content_file (key, filename, size_bytes) VALUES ('bad', 'file', 1.5)",
        "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES ('100', '200', 'INVALID', 'x')",
        "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES ('100', '200', 'CONTENT', 'file')",
        "INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (1, 3, 'bad', 0)",
        "INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (1, 2.5, 'bad', 1)",
        "INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (1, 3, 'bad', 1.5)",
        "INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (1, 3, ' ', 1)",
        "INSERT INTO shop_item_publication (item_id, item_revision, channel_id, message_id) VALUES (1, 3, '300', '400')",
        "INSERT INTO shop_item_publication (item_id, item_revision, channel_id, message_id) VALUES (1, 1, '300', '400')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', 1, 'UNKNOWN', 'unknown')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', 'oops', 'ADJUSTMENT', 'text')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', 1.5, 'ADJUSTMENT', 'real')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', 0, 'MESSAGE', 'zero')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', 1, 'TIMEOUT', 'positive')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key) VALUES ('100', '200', -1, 'PURCHASE', 'no-item')",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key, sale_of) VALUES ('100', '200', 100, 'SALE', 'duplicate-sale', 1)",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key, refund_of) VALUES ('100', '201', 100, 'REFUND', 'duplicate-refund', 1)",
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key, refund_of) VALUES ('100', '200', -100, 'SALE_REFUND', 'duplicate-sale-refund', 2)",
    ] {
        assert!(raw.execute_unprepared(invalid).await.is_err(), "{invalid}");
    }
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM current_transaction_reason WHERE code='MESSAGE' AND name='new' AND revision=2").await, 1);
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM current_shop_item WHERE id=1 AND revision=2 AND price=200 AND deleted_at IS NOT NULL").await, 1);
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM shop_item_revision").await,
        2
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 未対応の保存形式や終了した接続では操作できない() {
    assert_eq!(
        SqliteDatabaseAccess::open("").await.err(),
        Some(DatabaseError::InvalidData)
    );
    assert_eq!(
        SqliteDatabaseAccess::open(":memory:").await.err(),
        Some(DatabaseError::InvalidData)
    );
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let raw = inspect(&file.path()).await;
    raw.execute_unprepared("PRAGMA user_version = 3")
        .await
        .unwrap();
    assert_eq!(
        SqliteDatabaseAccess::open(file.path()).await.err(),
        Some(DatabaseError::InvalidData)
    );
    assert_eq!(number(&raw, "PRAGMA user_version").await, 3);
    raw.close().await.unwrap();
    let closed = db.clone();
    db.close().await.unwrap();
    assert_eq!(
        closed.begin_read().await.err(),
        Some(DatabaseError::Unavailable)
    );
}

fn admin() -> Actor {
    domain::Actor::new(guild(), user(), true)
}

fn target(value: i64) -> Balance {
    domain::Balance::new(value).unwrap()
}

#[tokio::test]
async fn 通話報酬は一分一回かつ日本時間で一日300回まで付与し再試行では増やさない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let other_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let other_not = NotRepository::new(other_db.clone());
    let start = UNIX_EPOCH + Duration::from_secs(54_000);
    let first = not
        .grant_voice_reward(buyer(), start)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.amount().get(), 1);
    assert_eq!(
        not.grant_voice_reward(buyer(), start + Duration::from_secs(59))
            .await
            .unwrap(),
        Some(first.clone())
    );
    let minute = start + Duration::from_secs(60);
    let (a, b) = tokio::join!(
        not.grant_voice_reward(buyer(), minute),
        other_not.grant_voice_reward(buyer(), minute)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    for minute in 2..300 {
        assert!(
            not.grant_voice_reward(buyer(), start + Duration::from_secs(minute * 60))
                .await
                .unwrap()
                .is_some()
        );
    }
    assert!(
        not.grant_voice_reward(buyer(), start + Duration::from_secs(300 * 60))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(300));
    other_db.close().await.unwrap();
    db.close().await.unwrap();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    assert!(
        not.grant_voice_reward(buyer(), start + Duration::from_secs(400 * 60))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        not.grant_voice_reward(buyer(), start).await.unwrap(),
        Some(first)
    );
    let midnight = start + Duration::from_secs(86_400);
    assert!(
        not.grant_voice_reward(buyer(), midnight - Duration::from_secs(1))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        not.grant_voice_reward(buyer(), midnight)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(301));
    let other_guild =
        domain::Actor::new(domain::GuildId::new(101).unwrap(), buyer().user_id(), false);
    assert!(
        not.grant_voice_reward(other_guild, start)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        not.grant_voice_reward(admin(), start)
            .await
            .unwrap()
            .is_some()
    );
    let raw = inspect(&file.path()).await;
    for fail_commit in [false, true] {
        raw.execute_unprepared(if fail_commit {
            "CREATE TABLE voice_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_voice AFTER INSERT ON not_transaction WHEN NEW.event_key LIKE 'voice:%'
             BEGIN INSERT INTO voice_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_voice BEFORE INSERT ON not_transaction WHEN NEW.event_key LIKE 'voice:%'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        }).await.unwrap();
        let earned = midnight + Duration::from_secs(if fail_commit { 120 } else { 60 });
        let before = not.get_balance(buyer()).await.unwrap();
        assert!(not.grant_voice_reward(buyer(), earned).await.is_err());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), before);
        raw.execute_unprepared("DROP TRIGGER reject_voice")
            .await
            .unwrap();
        assert!(
            not.grant_voice_reward(buyer(), earned)
                .await
                .unwrap()
                .is_some()
        );
    }
    not.adjust_balance(admin(), user(), target(i64::MAX), "voice-overflow")
        .await
        .unwrap();
    assert_eq!(
        not.grant_voice_reward(admin(), midnight).await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    );
    assert_eq!(
        not.grant_voice_reward(buyer(), UNIX_EPOCH - Duration::from_secs(1))
            .await,
        Err(RepositoryError::InvalidRewardTime)
    );
    assert_eq!(number(&raw, "PRAGMA user_version").await, 2);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 報酬は日本時間の回数と間隔を守り再起動後も付与と見送りの結果を保つ() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    not.adjust_balance(admin(), user(), target(41), "legacy")
        .await
        .unwrap();
    let raw = inspect(&file.path()).await;
    // v1の既存DBへ移行する経路も、同じ保存済み履歴で検証する。
    raw.execute_unprepared("DROP TABLE reward_event; PRAGMA user_version = 1")
        .await
        .unwrap();
    db.close().await.unwrap();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    assert_eq!(number(&raw, "PRAGMA user_version").await, 2);
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(41));
    let start = UNIX_EPOCH + Duration::from_secs(54_000); // 日本時間の00:00
    let first = not
        .grant_reward(admin(), LedgerReason::Message, "message-0", start)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.amount().get(), 5);
    assert!(
        not.grant_reward(
            admin(),
            LedgerReason::Message,
            "too-soon",
            start + Duration::from_millis(59_999)
        )
        .await
        .unwrap()
        .is_none()
    );
    for index in 1..100 {
        assert!(
            not.grant_reward(
                admin(),
                LedgerReason::Message,
                &format!("message-{index}"),
                start + Duration::from_secs(index * 60)
            )
            .await
            .unwrap()
            .is_some()
        );
    }
    assert!(
        not.grant_reward(
            admin(),
            LedgerReason::Message,
            "daily-cap",
            start + Duration::from_secs(6000)
        )
        .await
        .unwrap()
        .is_none()
    );
    let midnight = start + Duration::from_secs(86_400);
    assert!(
        not.grant_reward(admin(), LedgerReason::Message, "next-day", midnight)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        not.grant_reward(
            admin(),
            LedgerReason::Reaction,
            "independent-reason",
            midnight
        )
        .await
        .unwrap()
        .is_some()
    );
    let other = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), false);
    assert!(
        not.grant_reward(other, LedgerReason::Message, "message-0", start)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        not.grant_reward(buyer(), LedgerReason::Message, "message-0", start)
            .await
            .is_err()
    );
    assert!(
        not.grant_reward(buyer(), LedgerReason::Message, "independent-user", start)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        not.grant_reward(admin(), LedgerReason::Adjustment, "invalid", start)
            .await,
        Err(RepositoryError::InvalidRewardReason)
    );
    assert_eq!(
        not.grant_reward(
            admin(),
            LedgerReason::Message,
            "invalid-time",
            UNIX_EPOCH - Duration::from_secs(1)
        )
        .await,
        Err(RepositoryError::InvalidRewardTime)
    );
    // 日付が変わっても60秒の間隔はリセットしない。
    not.grant_reward(
        buyer(),
        LedgerReason::Reaction,
        "before-midnight",
        midnight - Duration::from_millis(1),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        not.grant_reward(buyer(), LedgerReason::Reaction, "at-midnight", midnight)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        not.grant_reward(
            buyer(),
            LedgerReason::Reaction,
            "after-interval",
            midnight + Duration::from_millis(59_999)
        )
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(547));
    // 変更前に付与済みの1 NOTは再起動・再配信でも増額しない。
    raw.execute_unprepared(
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key)
         VALUES ('102', '200', 1, 'MESSAGE', 'reward:MESSAGE:legacy-message');
         INSERT INTO reward_event (guild_id, event_key, user_id, reason, received_at, entry_id)
         SELECT guild_id, event_key, user_id, reason, 0, id FROM not_transaction
         WHERE guild_id = '102' AND event_key = 'reward:MESSAGE:legacy-message';",
    )
    .await
    .unwrap();
    raw.close().await.unwrap();
    db.close().await.unwrap();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    assert_eq!(
        not.grant_reward(admin(), LedgerReason::Message, "message-0", midnight)
            .await
            .unwrap(),
        Some(first)
    );
    for key in ["too-soon", "daily-cap"] {
        assert!(
            not.grant_reward(
                admin(),
                LedgerReason::Message,
                key,
                midnight + Duration::from_secs(600)
            )
            .await
            .unwrap()
            .is_none()
        );
    }
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(547));
    let legacy = domain::Actor::new(domain::GuildId::new(102).unwrap(), user(), false);
    let restored = not
        .grant_reward(legacy, LedgerReason::Message, "legacy-message", midnight)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.amount().get(), 1);
    assert_eq!(not.get_balance(legacy).await.unwrap(), target(1));
    not.grant_reward(legacy, LedgerReason::Message, "new-message", midnight)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(not.get_balance(legacy).await.unwrap(), target(6));
    db.close().await.unwrap();
}

#[tokio::test]
async fn 報酬の取引と処理済み記録は一括確定し売買や返金との競合でも重複しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let other_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let other_not = NotRepository::new(other_db.clone());
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let raw = inspect(&file.path()).await;
    for fail_commit in [false, true] {
        raw.execute_unprepared(if fail_commit {
            "CREATE TABLE reward_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_reward AFTER INSERT ON reward_event
             BEGIN INSERT INTO reward_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_reward BEFORE INSERT ON reward_event
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        }).await.unwrap();
        assert!(
            not.grant_reward(buyer(), LedgerReason::Message, "retry", purchase_time())
                .await
                .is_err()
        );
        assert_eq!(number(&raw, "SELECT COUNT(*) FROM reward_event").await, 0);
        assert_eq!(
            number(
                &raw,
                "SELECT COUNT(*) FROM not_transaction WHERE reason = 'MESSAGE'"
            )
            .await,
            0
        );
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
        raw.execute_unprepared("DROP TRIGGER reject_reward")
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        not.grant_reward(buyer(), LedgerReason::Message, "retry", purchase_time()),
        other_not.grant_reward(buyer(), LedgerReason::Message, "retry", purchase_time()),
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let next = purchase_time() + Duration::from_secs(60);
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "with-reward", next)
        .await
        .unwrap();
    let (a, b, purchase) = tokio::join!(
        not.grant_reward(buyer(), LedgerReason::Message, "concurrent-a", next),
        other_not.grant_reward(buyer(), LedgerReason::Message, "concurrent-b", next),
        shop.purchase(buyer(), &quote, next),
    );
    assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
    let purchase = purchase.unwrap();
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(710));
    let (reward, refund) = tokio::join!(
        other_not.grant_reward(buyer(), LedgerReason::Reaction, "with-refund", next),
        shop.refund(admin(), purchase.buyer().id(), "with-reward"),
    );
    assert!(reward.unwrap().is_some());
    refund.unwrap();
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1211));
    not.adjust_balance(admin(), user(), target(i64::MAX), "upper-bound")
        .await
        .unwrap();
    assert_eq!(
        not.grant_reward(admin(), LedgerReason::Message, "overflow", next)
            .await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    );
    assert_eq!(
        number(
            &raw,
            "SELECT COUNT(*) FROM reward_event WHERE event_key = 'reward:MESSAGE:overflow'"
        )
        .await,
        0
    );
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_balance WHERE balance < 0").await,
        0
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
    other_db.close().await.unwrap();
}

#[tokio::test]
async fn 調整と譲渡は同じ依頼を再反映せず失敗しても片側の履歴を残さない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let sender = domain::Actor::new(guild(), user(), false);
    let recipient = domain::UserId::new(201).unwrap();
    assert_eq!(
        not.adjust_amount(sender, recipient, domain::NotAmount::new(0), "zero")
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    let zero = not
        .adjust_amount(admin(), recipient, domain::NotAmount::new(0), "zero")
        .await
        .unwrap();
    assert_eq!(zero.amount().get(), 0);
    not.adjust_amount(
        admin(),
        recipient,
        domain::NotAmount::new(50),
        "seed-recipient",
    )
    .await
    .unwrap();
    assert_eq!(
        not.adjust_amount(admin(), recipient, domain::NotAmount::new(0), "zero")
            .await
            .unwrap(),
        zero
    );
    assert_eq!(
        not.adjust_amount(admin(), recipient, domain::NotAmount::new(-51), "overdraft")
            .await,
        Err(RepositoryError::Money(MoneyError::NegativeBalance))
    );
    assert_eq!(
        not.adjust_amount(admin(), recipient, domain::NotAmount::new(-50), "fifty")
            .await
            .unwrap()
            .amount()
            .get(),
        -50
    );
    assert_eq!(
        not.adjust_amount(admin(), user(), domain::NotAmount::new(-50), "fifty")
            .await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(
        not.adjust_amount(admin(), recipient, domain::NotAmount::new(-49), "fifty")
            .await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(
        not.adjust_amount(sender, recipient, domain::NotAmount::new(-50), "fifty")
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    not.adjust_balance(admin(), user(), target(1200), "seed-sender")
        .await
        .unwrap();
    assert_eq!(
        not.adjust_amount(admin(), user(), domain::NotAmount::new(-100), "hundred")
            .await
            .unwrap()
            .amount()
            .get(),
        -100
    );
    assert_eq!(
        not.transfer(sender, user(), 100, "self").await,
        Err(RepositoryError::SelfTransfer)
    );
    assert_eq!(
        not.transfer(sender, recipient, 101, "invalid").await,
        Err(RepositoryError::InvalidTransferAmount)
    );
    let first = not.transfer(sender, recipient, 100, "send").await.unwrap();
    assert_eq!(
        (first.0.amount().get(), first.1.amount().get()),
        (-100, 100)
    );
    assert_eq!(
        not.transfer(sender, recipient, 500, "send").await,
        Err(RepositoryError::RequestConflict)
    );
    let raw = inspect(&file.path()).await;
    for fail_commit in [false, true] {
        raw.execute_unprepared(if fail_commit {
            "CREATE TABLE transfer_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_transfer AFTER INSERT ON not_transaction
             WHEN NEW.reason = 'TIP_RECEIVED' OR NEW.event_key = 'adjust-amount:retry-adjust'
             BEGIN INSERT INTO transfer_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_transfer BEFORE INSERT ON not_transaction
             WHEN NEW.reason = 'TIP_RECEIVED' OR NEW.event_key = 'adjust-amount:retry-adjust'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        }).await.unwrap();
        assert!(not.transfer(sender, recipient, 500, "retry").await.is_err());
        assert!(
            not.adjust_amount(admin(), user(), domain::NotAmount::new(50), "retry-adjust")
                .await
                .is_err()
        );
        assert_eq!(not.get_balance(sender).await.unwrap(), target(1000));
        assert_eq!(
            number(
                &raw,
                "SELECT COUNT(*) FROM not_transaction WHERE event_key LIKE 'transfer:retry:%'"
            )
            .await,
            0
        );
        raw.execute_unprepared("DROP TRIGGER reject_transfer")
            .await
            .unwrap();
    }
    not.adjust_amount(admin(), user(), domain::NotAmount::new(50), "retry-adjust")
        .await
        .unwrap();
    not.adjust_amount(admin(), user(), domain::NotAmount::new(-50), "restore")
        .await
        .unwrap();
    not.transfer(sender, recipient, 1000, "last").await.unwrap();
    assert_eq!(
        not.transfer(sender, recipient, 100, "overdraft").await,
        Err(RepositoryError::InsufficientBalance)
    );
    assert_eq!(
        not.transfer(sender, recipient, 100, "send").await.unwrap(),
        first
    );
    not.adjust_balance(admin(), user(), target(500), "refill")
        .await
        .unwrap();
    not.adjust_balance(admin(), recipient, target(i64::MAX), "overflow-recipient")
        .await
        .unwrap();
    assert!(matches!(
        not.transfer(sender, recipient, 500, "overflow").await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    ));
    assert_eq!(not.get_balance(sender).await.unwrap(), target(500));
    assert_eq!(
        not.adjust_amount(
            admin(),
            recipient,
            domain::NotAmount::new(1),
            "adjust-overflow"
        )
        .await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    );
    assert_eq!(
        not.adjust_amount(
            admin(),
            recipient,
            domain::NotAmount::new(i64::MIN),
            "adjust-min"
        )
        .await,
        Err(RepositoryError::Money(MoneyError::NegativeBalance))
    );
    // 片側だけの不正履歴も、再試行で補完せず拒否する。
    let mut write = db.begin_write().await.unwrap();
    write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                user(),
                domain::NotAmount::new(-100),
                LedgerReason::TipSent,
                "transfer:broken:sent",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    assert_eq!(
        not.transfer(sender, recipient, 100, "broken").await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    assert_eq!(
        not.transfer(sender, recipient, 100, "send").await.unwrap(),
        first
    );
    assert_eq!(
        not.adjust_amount(admin(), recipient, domain::NotAmount::new(0), "zero")
            .await
            .unwrap(),
        zero
    );
    db.close().await.unwrap();
}

#[tokio::test]
async fn 譲渡と購入や返金が競合しても残高不足と重複した送金を防ぐ() {
    let file = TestDatabase::new();
    let first_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let second_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let first = NotRepository::new(first_db.clone());
    let second = NotRepository::new(second_db.clone());
    let (shop, item) = purchase_setup(&file, &first_db, 1200).await;
    let recipient = domain::UserId::new(202).unwrap();
    let (a, b) = tokio::join!(
        first.transfer(buyer(), recipient, 100, "same"),
        second.transfer(buyer(), recipient, 100, "same"),
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "purchase", purchase_time())
        .await
        .unwrap();
    let (transfer, purchase, adjustment, retry) = tokio::join!(
        second.transfer(buyer(), recipient, 1000, "competing"),
        shop.purchase(buyer(), &quote, purchase_time()),
        first.adjust_amount(
            admin(),
            buyer().user_id(),
            domain::NotAmount::new(-100),
            "competing-adjustment"
        ),
        second.adjust_amount(
            admin(),
            buyer().user_id(),
            domain::NotAmount::new(-100),
            "competing-adjustment"
        ),
    );
    let adjustment = adjustment.unwrap();
    assert_eq!(adjustment, retry.unwrap());
    assert_eq!(adjustment.amount().get(), -100);
    assert_ne!(transfer.is_ok(), purchase.is_ok());
    if purchase.is_err() {
        first
            .adjust_balance(admin(), buyer().user_id(), target(500), "refill")
            .await
            .unwrap();
    }
    let purchase = shop
        .purchase(buyer(), &quote, purchase_time())
        .await
        .unwrap();
    let seller = domain::Actor::new(guild(), user(), false);
    let (sent, refund) = tokio::join!(
        second.transfer(seller, recipient, 500, "spent-sale"),
        shop.refund(admin(), purchase.buyer().id(), "competing-refund"),
    );
    assert_ne!(sent.is_ok(), refund.is_ok());
    assert_eq!(first.get_balance(seller).await.unwrap(), target(0));
    let raw = inspect(&file.path()).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_balance WHERE balance < 0").await,
        0
    );
    assert_eq!(
        number(
            &raw,
            "SELECT SUM(amount) FROM not_transaction WHERE reason IN ('TIP_SENT', 'TIP_RECEIVED')"
        )
        .await,
        0
    );
    assert_eq!(
        number(
            &raw,
            "SELECT COUNT(*) FROM not_transaction WHERE event_key LIKE 'transfer:same:%'"
        )
        .await,
        2
    );
    assert_eq!(trade_count(&raw).await, 2);
    raw.close().await.unwrap();
    first_db.close().await.unwrap();
    second_db.close().await.unwrap();
}

#[tokio::test]
async fn 残高とランキングは実行者のサーバーに所属する履歴だけを集計する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let viewer = domain::Actor::new(guild(), user(), false);
    assert_eq!(not.get_balance(viewer).await.unwrap(), target(0));
    assert!(not.get_ranking(viewer, 10).await.unwrap().is_empty());
    for (id, amount, key) in [
        (2, 100, "two"),
        (10, 100, "ten"),
        (200, 200, "top"),
        (3, 0, "zero"),
    ] {
        not.adjust_balance(
            admin(),
            domain::UserId::new(id).unwrap(),
            target(amount),
            key,
        )
        .await
        .unwrap();
    }
    let other = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), true);
    not.adjust_balance(other, user(), target(999), "top")
        .await
        .unwrap();
    assert_eq!(not.get_balance(viewer).await.unwrap(), target(200));
    assert_eq!(not.get_balance(other).await.unwrap(), target(999));
    let rank = not.get_ranking(viewer, 10).await.unwrap();
    let values: Vec<_> = rank
        .into_iter()
        .map(|row| (row.user_id().get(), row.balance().get()))
        .collect();
    // DB設計のTEXT IDの昇順。同額なら "10" が "2" より先になる。
    assert_eq!(values, [(200, 200), (10, 100), (2, 100), (3, 0)]);
    assert_eq!(not.get_ranking(viewer, 2).await.unwrap().len(), 2);
    assert!(not.get_ranking(viewer, 0).await.unwrap().is_empty());
    assert_eq!(not.get_ranking(other, 10).await.unwrap().len(), 1);
    db.close().await.unwrap();
}

#[tokio::test]
async fn 目標残高への調整は差額だけ記録し再起動後の再試行でも元の結果を返す() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let first = not
        .adjust_balance(admin(), user(), target(100), "first")
        .await
        .unwrap();
    assert_eq!(first.amount().get(), 100);
    assert_eq!(first.reason(), LedgerReason::Adjustment);
    assert_eq!(first.event_key(), "adjust:first");
    let decrease = not
        .adjust_balance(admin(), user(), target(40), "decrease")
        .await
        .unwrap();
    assert_eq!(decrease.amount().get(), -60);
    let zero = not
        .adjust_balance(admin(), user(), target(40), "zero")
        .await
        .unwrap();
    assert_eq!(zero.amount().get(), 0);
    assert_eq!(
        not.adjust_balance(admin(), user(), target(7), "later")
            .await
            .unwrap()
            .amount()
            .get(),
        -33
    );
    drop(not);
    db.close().await.unwrap();

    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    assert_eq!(
        not.adjust_balance(admin(), user(), target(100), "first")
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        not.adjust_balance(admin(), user(), target(40), "zero")
            .await
            .unwrap(),
        zero
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(7));
    assert_eq!(
        not.adjust_balance(admin(), user(), target(99), "first")
            .await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(
        not.adjust_balance(
            admin(),
            domain::UserId::new(201).unwrap(),
            target(100),
            "first"
        )
        .await,
        Err(RepositoryError::RequestConflict)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        4
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 権限不足や不正な調整と同じ依頼の内容変更は履歴を増やさない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let ordinary = domain::Actor::new(guild(), user(), false);
    assert_eq!(
        not.adjust_balance(ordinary, user(), target(100), "new")
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    not.adjust_balance(admin(), user(), target(20), "existing")
        .await
        .unwrap();
    assert_eq!(
        not.adjust_balance(ordinary, user(), target(20), "existing")
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    for key in ["", "  ", "bad\nkey", "bad\0key"] {
        assert_eq!(
            not.adjust_balance(admin(), user(), target(100), key).await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidEventKey
            ))
        );
    }
    // 他の理由の行へ同じキーが使われていても、調整の成功として返さない。
    let mut write = db.begin_write().await.unwrap();
    write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                user(),
                domain::NotAmount::new(1),
                LedgerReason::Message,
                "adjust:collision",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    assert_eq!(
        not.adjust_balance(admin(), user(), target(21), "collision")
            .await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(21));
    let raw = inspect(&file.path()).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        2
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 同時の残高調整でも依頼を重複反映せず差額を順番に計算する() {
    for (second_key, second_target) in [("same", 100), ("same", 50), ("different", 100)] {
        let file = TestDatabase::new();
        let first_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let second_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let first = NotRepository::new(first_db.clone());
        let second = NotRepository::new(second_db.clone());
        let (left, right) = tokio::join!(
            first.adjust_balance(admin(), user(), target(100), "same"),
            second.adjust_balance(admin(), user(), target(second_target), second_key),
        );
        let expected_count = if second_key == "different" {
            let (left, right) = (left.unwrap(), right.unwrap());
            let mut amounts = [left.amount().get(), right.amount().get()];
            amounts.sort();
            assert_eq!(amounts, [0, 100]);
            assert_ne!(left.id(), right.id());
            2
        } else if second_target == 100 {
            assert_eq!(left.unwrap(), right.unwrap());
            1
        } else {
            match (left, right) {
                (Ok(entry), Err(RepositoryError::RequestConflict)) => {
                    assert_eq!(entry.amount().get(), 100)
                }
                (Err(RepositoryError::RequestConflict), Ok(entry)) => {
                    assert_eq!(entry.amount().get(), 50)
                }
                other => panic!("expected one success and one conflict: {other:?}"),
            }
            1
        };
        let raw = inspect(&file.path()).await;
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
            expected_count
        );
        let current = first.get_balance(admin()).await.unwrap().get();
        assert!(current == 100 || (second_target == 50 && current == 50));
        raw.close().await.unwrap();
        first_db.close().await.unwrap();
        second_db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 調整は金額の上下限を守り取引番号の採番上限後は履歴を追加しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let first = not
        .adjust_balance(admin(), user(), target(i64::MAX), "max")
        .await
        .unwrap();
    assert_eq!(first.amount().get(), i64::MAX);
    assert_eq!(
        not.adjust_balance(admin(), user(), target(0), "empty")
            .await
            .unwrap()
            .amount()
            .get(),
        -i64::MAX
    );
    not.adjust_balance(admin(), user(), target(i64::MAX), "max-again")
        .await
        .unwrap();
    assert_eq!(
        not.adjust_balance(admin(), user(), target(i64::MAX), "max")
            .await
            .unwrap(),
        first
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(i64::MAX));

    let raw = inspect(&file.path()).await;
    // SQLiteのROWID上限でのランダム採番を防ぐ。追記順を崩す行は作らない。
    raw.execute_unprepared("INSERT INTO not_transaction (id, guild_id, user_id, amount, reason, event_key) VALUES (9223372036854775807, '100', '200', 0, 'ADJUSTMENT', 'id-limit')").await.unwrap();
    assert_eq!(
        not.adjust_balance(admin(), user(), target(0), "after-id-limit")
            .await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(i64::MAX));
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        4
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 調整の履歴追加に失敗したら残高を変えずに失敗を返す() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    not.adjust_balance(admin(), user(), target(20), "initial")
        .await
        .unwrap();
    let raw = inspect(&file.path()).await;
    raw.execute_unprepared(
        "CREATE TRIGGER reject_adjustment BEFORE INSERT ON not_transaction
        WHEN NEW.event_key = 'adjust:reject'
        BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
    )
    .await
    .unwrap();
    assert_eq!(
        not.adjust_balance(admin(), user(), target(100), "reject")
            .await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(20));
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        1
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn ランキングや取引の照会では不正な保存値を有効な値として扱わない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let not = NotRepository::new(db.clone());
    let raw = inspect(&file.path()).await;
    raw.execute_unprepared(
        "INSERT INTO not_transaction (guild_id, user_id, amount, reason, event_key)
        VALUES ('100', 'invalid-id', 1, 'ADJUSTMENT', 'adjust:bad-id'),
               ('101', '200', -1, 'ADJUSTMENT', 'negative');",
    )
    .await
    .unwrap();
    assert_eq!(
        not.get_ranking(admin(), 10).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(
        not.adjust_balance(admin(), user(), target(1), "bad-id")
            .await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    let other = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), false);
    assert_eq!(
        not.get_ranking(other, 10).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(
        not.get_balance(other).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        2
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

fn details(name: &str, price: i64) -> ItemDetails {
    domain::ItemDetails::new(name, "商品説明\n二行目", domain::Price::new(price).unwrap()).unwrap()
}

fn saved_content(file: &TestDatabase, actor: Actor, request: &str) -> StoredContent {
    DiscordShopRepository::new(file.0.join("contents"))
        .store_content(actor, request, "資料.pdf", "file contents".as_bytes())
        .unwrap()
}

#[test]
fn コンテンツは配送できるサイズまで読み取り完成したファイルを保持する() {
    let file = TestDatabase::new();
    let directory = file.0.join("contents");
    for (request, filename) in [
        ("../escape", "file"),
        ("/absolute", "file"),
        ("", "file"),
        ("ok", "../file"),
        ("ok", "/file"),
        ("ok", "a\\file"),
    ] {
        assert!(matches!(
            DiscordShopRepository::new(directory.clone()).store_content(
                admin(),
                request,
                filename,
                "abc".as_bytes()
            ),
            Err(ContentStorageError::Validation(_))
        ));
    }
    assert!(!directory.exists());
    assert!(matches!(
        DiscordShopRepository::new(directory.clone()).store_content(
            admin(),
            "empty",
            "file",
            std::io::empty()
        ),
        Err(ContentStorageError::Validation(
            ValidationError::InvalidFileSize
        ))
    ));
    let mut oversized = std::io::Read::take(std::io::repeat(0), MAX_CONTENT_BYTES + 10);
    assert!(matches!(
        DiscordShopRepository::new(directory.clone()).store_content(
            admin(),
            "oversized",
            "file",
            &mut oversized
        ),
        Err(ContentStorageError::TooLarge)
    ));
    assert_eq!(oversized.limit(), 9); // 上限+1バイトだけ読み、残りは消費しない。
    assert!(!directory.exists());

    let saved = saved_content(&file, admin(), "original");
    let path = directory.join(saved.file().key());
    assert_eq!(fs::read(&path).unwrap(), b"file contents");
    assert_eq!(
        saved_content(&file, admin(), "original").file(),
        saved.file()
    );
    for bytes in [b"other content".as_slice(), b"short"] {
        assert!(matches!(
            DiscordShopRepository::new(directory.clone()).store_content(
                admin(),
                "original",
                "資料.pdf",
                bytes
            ),
            Err(ContentStorageError::Conflict)
        ));
    }
    assert_eq!(fs::read(&path).unwrap(), b"file contents");
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    let max = DiscordShopRepository::new(directory.clone())
        .store_content(
            admin(),
            "maximum",
            "file.bin",
            std::io::Read::take(std::io::repeat(0), MAX_CONTENT_BYTES),
        )
        .unwrap();
    assert_eq!(max.file().size_bytes(), MAX_CONTENT_BYTES);
    assert_eq!(
        fs::metadata(directory.join(max.file().key()))
            .unwrap()
            .len(),
        MAX_CONTENT_BYTES
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn 保存先のリンクを拒否し同じ出品の同時保存では一つのファイルを確定する() {
    let file = TestDatabase::new();
    let directory = file.0.join("contents");
    let results = std::thread::scope(|scope| {
        let left = scope.spawn(|| saved_content(&file, admin(), "same"));
        let right = scope.spawn(|| saved_content(&file, admin(), "same"));
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_eq!(results.0.file(), results.1.file());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    let outside = file.0.join("outside");
    fs::write(&outside, b"file contents").unwrap();
    std::os::unix::fs::symlink(&outside, directory.join("content-100-200-symlink")).unwrap();
    assert!(matches!(
        DiscordShopRepository::new(directory.clone()).store_content(
            admin(),
            "symlink",
            "file",
            b"file contents".as_slice()
        ),
        Err(ContentStorageError::UnsafePath)
    ));
    assert_eq!(fs::read(outside).unwrap(), b"file contents");
    assert!(fs::read_dir(&directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pending-")
    }));
}

#[tokio::test]
async fn 商品を変更や削除して再起動しても識別情報と過去の版を保持する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let viewer = domain::Actor::new(guild(), domain::UserId::new(201).unwrap(), false);
    let stored = saved_content(&file, admin(), "create");
    let original_details = details("資料集", 500);
    let original = shop
        .create(admin(), original_details.clone(), stored.clone())
        .await
        .unwrap();
    assert_eq!(original.revision(), 1);
    assert_eq!(original.seller_id(), user());
    assert_eq!(
        shop.read(viewer, original.id()).await.unwrap().as_ref(),
        Some(&original)
    );
    assert_eq!(
        shop.list(viewer).await.unwrap(),
        std::slice::from_ref(&original)
    );
    let other = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), true);
    assert!(shop.list(other).await.unwrap().is_empty());
    assert_eq!(shop.read(other, original.id()).await, Ok(None));
    let changed = shop
        .update(admin(), original.id(), 1, details("新版", 700))
        .await
        .unwrap();
    assert_eq!(changed.revision(), 2);
    assert_eq!(changed.target(), original.target());
    assert_eq!(changed.guild_id(), original.guild_id());
    assert_eq!(changed.seller_id(), original.seller_id());
    assert_eq!(
        shop.read(viewer, original.id()).await.unwrap().as_ref(),
        Some(&changed)
    );
    let deleted = shop.delete(admin(), original.id(), 2).await.unwrap();
    assert!(deleted.is_deleted());
    assert_eq!(deleted.revision(), 3);
    assert_eq!(deleted.details(), changed.details());
    assert_eq!(
        shop.delete(admin(), original.id(), 2).await.unwrap(),
        deleted
    );
    assert!(shop.list(viewer).await.unwrap().is_empty());
    assert_eq!(shop.read(viewer, original.id()).await, Ok(None));
    let mut read = db.begin_read().await.unwrap();
    assert_eq!(
        read.find_item(guild(), original.id()).await.unwrap(),
        Some(deleted.clone())
    );
    assert_eq!(
        read.find_item_revision(guild(), original.id(), 1)
            .await
            .unwrap(),
        Some(original.clone())
    );
    read.commit().await.unwrap();
    drop(shop);
    db.close().await.unwrap();

    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    assert_eq!(
        shop.create(
            admin(),
            original_details,
            saved_content(&file, admin(), "create")
        )
        .await
        .unwrap(),
        deleted
    );
    assert_eq!(
        shop.update(admin(), original.id(), 1, changed.details().clone())
            .await
            .unwrap(),
        changed
    );
    assert_eq!(
        shop.update(admin(), original.id(), 3, details("削除後", 1))
            .await,
        Err(RepositoryError::ItemDeleted)
    );
    assert_eq!(
        fs::read(file.0.join("contents").join(stored.file().key())).unwrap(),
        b"file contents"
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM content_file").await, 1);
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM shop_item").await, 1);
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM shop_item_revision").await,
        3
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 商品変更では出品者と版を照合し出品の再試行では初版の内容を確認する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let stored = saved_content(&file, admin(), "create");
    let original = shop
        .create(admin(), details("original", 500), stored.clone())
        .await
        .unwrap();
    let stranger = domain::Actor::new(guild(), domain::UserId::new(201).unwrap(), true);
    let other_guild = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), true);
    for actor in [stranger, other_guild] {
        assert_eq!(
            shop.create(actor, details("original", 500), stored.clone())
                .await,
            Err(RepositoryError::PermissionDenied)
        );
        let expected = if actor.guild_id() == guild() {
            RepositoryError::PermissionDenied
        } else {
            RepositoryError::ItemNotFound
        };
        assert_eq!(
            shop.update(actor, original.id(), 1, details("hijack", 1))
                .await,
            Err(expected)
        );
        assert!(shop.delete(actor, original.id(), 1).await.is_err());
    }
    assert_eq!(
        shop.create(admin(), details("different", 500), stored.clone())
            .await,
        Err(RepositoryError::RequestConflict)
    );
    let renamed = DiscordShopRepository::new(file.0.join("contents"))
        .store_content(admin(), "create", "別名.pdf", b"file contents".as_slice())
        .unwrap();
    assert_eq!(
        shop.create(admin(), details("original", 500), renamed)
            .await,
        Err(RepositoryError::RequestConflict)
    );
    let second = shop
        .update(admin(), original.id(), 1, details("second", 600))
        .await
        .unwrap();
    let third = shop
        .update(admin(), original.id(), 2, details("third", 700))
        .await
        .unwrap();
    assert_eq!(
        shop.update(admin(), original.id(), 1, second.details().clone())
            .await
            .unwrap(),
        second
    );
    assert_eq!(
        shop.create(admin(), original.details().clone(), stored)
            .await
            .unwrap(),
        third
    );
    assert_eq!(
        shop.update(admin(), original.id(), 1, details("stale", 800))
            .await,
        Err(RepositoryError::RevisionConflict)
    );
    assert_eq!(
        shop.delete(admin(), original.id(), 1).await,
        Err(RepositoryError::RevisionConflict)
    );
    for revision in [0, -1, i64::MAX] {
        assert_eq!(
            shop.update(admin(), original.id(), revision, details("bad", 1))
                .await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidRevision
            ))
        );
        assert_eq!(
            shop.delete(admin(), original.id(), revision).await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidRevision
            ))
        );
    }
    assert_eq!(
        shop.update(admin(), original.id(), 10, details("future", 1))
            .await,
        Err(RepositoryError::RevisionConflict)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM shop_item_revision").await,
        3
    );
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM shop_item").await, 1);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 同時の出品や商品変更でも商品を重複作成せず他の版を上書きしない() {
    let file = TestDatabase::new();
    let first_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let second_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let first = ShopRepository::new(first_db.clone());
    let second = ShopRepository::new(second_db.clone());
    let stored = saved_content(&file, admin(), "concurrent");
    let (left, right) = tokio::join!(
        first.create(admin(), details("same", 500), stored.clone()),
        second.create(admin(), details("same", 500), stored)
    );
    let item = left.unwrap();
    assert_eq!(right.unwrap(), item);
    let (left, right) = tokio::join!(
        first.update(admin(), item.id(), 1, details("left", 600)),
        second.update(admin(), item.id(), 1, details("right", 700))
    );
    match (left, right) {
        (Ok(changed), Err(RepositoryError::RevisionConflict))
        | (Err(RepositoryError::RevisionConflict), Ok(changed)) => {
            assert_eq!(changed.revision(), 2)
        }
        result => panic!("expected one edit and one revision conflict: {result:?}"),
    }
    let raw = inspect(&file.path()).await;
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM content_file").await, 1);
    assert_eq!(number(&raw, "SELECT COUNT(*) FROM shop_item").await, 1);
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM shop_item_revision").await,
        2
    );
    raw.close().await.unwrap();
    first_db.close().await.unwrap();
    second_db.close().await.unwrap();
}

#[tokio::test]
async fn 商品登録に失敗したら全レコードを取り消して保存済みファイルは保持する() {
    for fail_commit in [false, true] {
        let file = TestDatabase::new();
        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let shop = ShopRepository::new(db.clone());
        let stored = saved_content(&file, admin(), "failed");
        let raw = inspect(&file.path()).await;
        let sql = if fail_commit {
            "CREATE TABLE commit_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_item AFTER INSERT ON shop_item_revision
             BEGIN INSERT INTO commit_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_item BEFORE INSERT ON shop_item_revision
             BEGIN SELECT RAISE(ABORT, 'injected revision failure'); END;"
        };
        raw.execute_unprepared(sql).await.unwrap();
        assert_eq!(
            shop.create(admin(), details("retry", 500), stored.clone())
                .await,
            Err(RepositoryError::Database(DatabaseError::InvalidData))
        );
        for table in ["content_file", "shop_item", "shop_item_revision"] {
            assert_eq!(
                number(&raw, &format!("SELECT COUNT(*) FROM {table}")).await,
                0
            );
        }
        assert_eq!(
            fs::read(file.0.join("contents").join(stored.file().key())).unwrap(),
            b"file contents"
        );
        raw.execute_unprepared("DROP TRIGGER reject_item")
            .await
            .unwrap();
        shop.create(
            admin(),
            details("retry", 500),
            saved_content(&file, admin(), "failed"),
        )
        .await
        .unwrap();
        assert_eq!(number(&raw, "SELECT COUNT(*) FROM shop_item").await, 1);
        assert_eq!(fs::read_dir(file.0.join("contents")).unwrap().count(), 1);
        raw.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 読取専用の取引や未保存のコンテンツ情報からは商品を登録できない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let stored = saved_content(&file, admin(), "valid");
    let item = shop
        .create(admin(), details("valid", 1), stored.clone())
        .await
        .unwrap();
    let mut read = db.begin_read().await.unwrap();
    assert_eq!(
        read.append_content_item(
            admin(),
            domain::NewShopItem::new(
                details("forbidden", 1),
                ItemTarget::Content(stored.file().clone())
            )
        )
        .await,
        Err(DatabaseError::ReadOnly)
    );
    assert_eq!(
        read.append_item_revision(item).await,
        Err(DatabaseError::ReadOnly)
    );
    read.commit().await.unwrap();
    let raw = inspect(&file.path()).await;
    raw.execute_unprepared("INSERT INTO shop_item (id, guild_id, seller_id, kind, target) VALUES (2, '100', '200', 'CONTENT', 'not-saved');
        INSERT INTO shop_item_revision (item_id, revision, name, price) VALUES (2, 1, 'broken', 1);").await.unwrap();
    assert_eq!(
        shop.read(admin(), domain::ItemId::new(2).unwrap()).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    assert_eq!(
        shop.list(admin()).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

fn buyer() -> Actor {
    domain::Actor::new(guild(), domain::UserId::new(201).unwrap(), false)
}

fn purchase_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

async fn purchase_setup(
    file: &TestDatabase,
    db: &SqliteDatabaseAccess,
    funds: i64,
) -> (ShopRepository, ShopItem) {
    NotRepository::new(db.clone())
        .adjust_balance(admin(), buyer().user_id(), target(funds), "seed-buyer")
        .await
        .unwrap();
    let shop = ShopRepository::new(db.clone());
    let item = shop
        .create(
            admin(),
            details("購入する資料", 500),
            saved_content(file, admin(), "purchase-item"),
        )
        .await
        .unwrap();
    (shop, item)
}

async fn trade_count(raw: &DatabaseConnection) -> i64 {
    number(
        raw,
        "SELECT COUNT(*) FROM not_transaction WHERE reason IN ('PURCHASE', 'SALE')",
    )
    .await
}

#[tokio::test]
async fn ロールの購入と返金は出品者の残高と売上履歴を変えない() {
    for seller_balance in [0, i64::MAX] {
        let file = TestDatabase::new();
        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let not = NotRepository::new(db.clone());
        not.adjust_balance(admin(), buyer().user_id(), target(1200), "fund-buyer")
            .await
            .unwrap();
        not.adjust_balance(
            admin(),
            admin().user_id(),
            target(seller_balance),
            "fund-seller",
        )
        .await
        .unwrap();
        let shop = ShopRepository::new(db.clone());
        let item = shop
            .create_role(
                admin(),
                domain::RoleId::new(500).unwrap(),
                details("会員権", 500),
            )
            .await
            .unwrap();
        let now = purchase_time();
        let quote = shop
            .prepare_purchase(buyer(), item.id(), "role", now)
            .await
            .unwrap();
        let paid = shop.purchase(buyer(), &quote, now).await.unwrap();
        assert!(paid.seller().is_none());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(700));
        assert_eq!(
            not.get_balance(admin()).await.unwrap(),
            target(seller_balance)
        );
        assert_eq!(shop.purchase(buyer(), &quote, now).await.unwrap(), paid);
        db.close().await.unwrap();

        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let shop = ShopRepository::new(db.clone());
        assert_eq!(
            shop.find_purchase_by_confirmation(buyer(), "role")
                .await
                .unwrap(),
            Some(paid.clone())
        );
        assert_eq!(
            shop.list_purchases(buyer()).await.unwrap(),
            vec![paid.clone()]
        );
        assert_eq!(
            shop.get_purchase(buyer(), paid.buyer().id()).await.unwrap(),
            paid
        );
        assert_eq!(
            shop.purchase(buyer(), &quote, quote.expires_at())
                .await
                .unwrap(),
            paid
        );
        let refund = shop
            .refund(admin(), paid.buyer().id(), "return-role")
            .await
            .unwrap();
        assert!(refund.seller().is_none());
        assert_eq!(refund.buyer().amount().get(), 500);
        assert_eq!(
            shop.refund(admin(), paid.buyer().id(), "return-role")
                .await
                .unwrap(),
            refund
        );
        assert_eq!(
            shop.refund(admin(), paid.buyer().id(), "another-return")
                .await,
            Err(RepositoryError::AlreadyRefunded)
        );
        assert_eq!(
            shop.get_purchase(buyer(), paid.buyer().id()).await,
            Err(RepositoryError::AlreadyRefunded)
        );
        assert!(shop.list_purchases(buyer()).await.unwrap().is_empty());
        assert_eq!(
            shop.purchase(buyer(), &quote, quote.expires_at())
                .await
                .unwrap(),
            paid
        );
        let not = NotRepository::new(db.clone());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
        assert_eq!(
            not.get_balance(admin()).await.unwrap(),
            target(seller_balance)
        );
        let raw = inspect(&file.path()).await;
        assert_eq!(trade_count(&raw).await, 1);
        assert_eq!(
            number(
                &raw,
                "SELECT COUNT(*) FROM not_transaction WHERE reason IN ('SALE', 'SALE_REFUND')"
            )
            .await,
            0
        );
        assert_eq!(
            number(
                &raw,
                "SELECT COUNT(*) FROM not_transaction WHERE reason = 'REFUND'"
            )
            .await,
            1
        );
        raw.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 旧仕様の売上付きロール購入も再課金せず元の売上とともに返金できる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    NotRepository::new(db.clone())
        .adjust_balance(admin(), buyer().user_id(), target(1200), "fund")
        .await
        .unwrap();
    let shop = ShopRepository::new(db.clone());
    let item = shop
        .create_role(
            admin(),
            domain::RoleId::new(500).unwrap(),
            details("旧仕様の会員権", 500),
        )
        .await
        .unwrap();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "legacy-role", purchase_time())
        .await
        .unwrap();
    let mut write = db.begin_write().await.unwrap();
    let purchase = write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                buyer().user_id(),
                domain::NotAmount::new(-500),
                LedgerReason::Purchase(item.id()),
                "content:legacy-role:buyer",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let sale = write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                admin().user_id(),
                domain::NotAmount::new(500),
                LedgerReason::Sale(purchase.id()),
                "content:legacy-role:seller",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let receipt = shop
        .find_purchase_by_confirmation(buyer(), "legacy-role")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.buyer(), &purchase);
    assert_eq!(receipt.seller(), Some(&sale));
    assert_eq!(
        shop.purchase(buyer(), &quote, quote.expires_at())
            .await
            .unwrap(),
        receipt
    );
    let not = NotRepository::new(db.clone());
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(700));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
    let refund = shop
        .refund(admin(), purchase.id(), "legacy-return")
        .await
        .unwrap();
    assert_eq!(refund.buyer().amount().get(), 500);
    assert_eq!(refund.seller().unwrap().amount().get(), -500);
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(0));
    assert_eq!(
        shop.refund(admin(), purchase.id(), "legacy-return")
            .await
            .unwrap(),
        refund
    );
    db.close().await.unwrap();
}

#[tokio::test]
async fn 購入確認では課金せず確定済みの確認は商品変更や再起動後も再課金しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let now = purchase_time();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "confirmed", now)
        .await
        .unwrap();
    assert_eq!(quote.id(), "confirmed");
    assert_eq!(quote.buyer_id(), buyer().user_id());
    assert_eq!(quote.item(), &item);
    assert_eq!(quote.expires_at(), now + Duration::from_secs(300));
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    assert_eq!(
        NotRepository::new(db.clone())
            .get_balance(buyer())
            .await
            .unwrap(),
        target(1200)
    );
    let result = shop.purchase(buyer(), &quote, now).await.unwrap();
    assert_eq!(result.buyer().amount().get(), -500);
    assert_eq!(result.buyer().reason(), LedgerReason::Purchase(item.id()));
    assert_eq!(result.buyer().event_key(), "content:confirmed:buyer");
    assert_eq!(result.seller().unwrap().amount().get(), 500);
    assert_eq!(
        result.seller().unwrap().reason(),
        LedgerReason::Sale(result.buyer().id())
    );
    assert_eq!(
        result.seller().unwrap().event_key(),
        "content:confirmed:seller"
    );
    let not = NotRepository::new(db.clone());
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(700));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
    assert_eq!(
        number(
            &raw,
            "SELECT SUM(amount) FROM not_transaction WHERE reason IN ('PURCHASE','SALE')"
        )
        .await,
        0
    );
    assert_eq!(shop.purchase(buyer(), &quote, now).await.unwrap(), result);
    assert_eq!(
        shop.prepare_purchase(buyer(), item.id(), "new-confirmation", now)
            .await,
        Err(RepositoryError::AlreadyPurchased)
    );

    shop.update(admin(), item.id(), 1, details("値上げ", 900))
        .await
        .unwrap();
    not.adjust_balance(admin(), buyer().user_id(), target(0), "spent-after")
        .await
        .unwrap();
    let replay = shop
        .purchase(buyer(), &quote, quote.expires_at())
        .await
        .unwrap();
    assert_eq!(replay.buyer(), result.buyer());
    assert_eq!(replay.seller(), result.seller());
    assert_eq!(replay.item().details().price().get(), 900);
    shop.delete(admin(), item.id(), 2).await.unwrap();
    raw.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let replay = ShopRepository::new(db.clone())
        .purchase(buyer(), &quote, now + Duration::from_secs(600))
        .await
        .unwrap();
    assert!(replay.item().is_deleted());
    assert_eq!(replay.buyer(), result.buyer());
    assert_eq!(replay.seller(), result.seller());
    assert_eq!(
        NotRepository::new(db.clone())
            .get_balance(buyer())
            .await
            .unwrap(),
        target(0)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 2);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 販売していない商品や購入条件を満たさない商品には購入確認を発行しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 499).await;
    let now = purchase_time();
    for key in ["", " ", "bad\nkey"] {
        assert_eq!(
            shop.prepare_purchase(buyer(), item.id(), key, now).await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidEventKey
            ))
        );
    }
    assert_eq!(
        shop.prepare_purchase(buyer(), item.id(), "poor", now).await,
        Err(RepositoryError::InsufficientBalance)
    );
    assert_eq!(
        shop.prepare_purchase(admin(), item.id(), "self", now).await,
        Err(RepositoryError::SelfPurchase)
    );
    let other = domain::Actor::new(domain::GuildId::new(101).unwrap(), buyer().user_id(), false);
    assert_eq!(
        shop.prepare_purchase(other, item.id(), "guild", now).await,
        Err(RepositoryError::ItemNotFound)
    );
    assert_eq!(
        shop.prepare_purchase(buyer(), domain::ItemId::new(999).unwrap(), "missing", now)
            .await,
        Err(RepositoryError::ItemNotFound)
    );
    shop.delete(admin(), item.id(), 1).await.unwrap();
    assert_eq!(
        shop.prepare_purchase(buyer(), item.id(), "deleted", now)
            .await,
        Err(RepositoryError::ItemDeleted)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 購入確認の期限と本人とサーバーを照合するまでは課金しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let now = purchase_time();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "expires", now)
        .await
        .unwrap();
    let actors = [
        admin(),
        domain::Actor::new(domain::GuildId::new(101).unwrap(), buyer().user_id(), false),
    ];
    for actor in actors {
        assert_eq!(
            shop.purchase(actor, &quote, now).await,
            Err(RepositoryError::PermissionDenied)
        );
    }
    assert_eq!(
        shop.purchase(buyer(), &quote, quote.expires_at()).await,
        Err(RepositoryError::QuoteExpired)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    shop.purchase(
        buyer(),
        &quote,
        quote.expires_at() - Duration::from_nanos(1),
    )
    .await
    .unwrap();
    for actor in actors {
        assert_eq!(
            shop.purchase(actor, &quote, now).await,
            Err(RepositoryError::PermissionDenied)
        );
    }
    assert_eq!(trade_count(&raw).await, 2);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 価格を元に戻しても確認後に商品版が変わっていれば購入を拒否する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let now = purchase_time();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "old-price", now)
        .await
        .unwrap();
    shop.update(admin(), item.id(), 1, details("new", 600))
        .await
        .unwrap();
    assert_eq!(
        shop.purchase(buyer(), &quote, now).await,
        Err(RepositoryError::RevisionConflict)
    );
    shop.update(admin(), item.id(), 2, details("restored-price", 500))
        .await
        .unwrap();
    assert_eq!(
        shop.purchase(buyer(), &quote, now).await,
        Err(RepositoryError::RevisionConflict)
    );
    let fresh = shop
        .prepare_purchase(buyer(), item.id(), "fresh", now)
        .await
        .unwrap();
    shop.delete(admin(), item.id(), 3).await.unwrap();
    assert_eq!(
        shop.purchase(buyer(), &fresh, now).await,
        Err(RepositoryError::RevisionConflict)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    assert_eq!(
        NotRepository::new(db.clone())
            .get_balance(buyer())
            .await
            .unwrap(),
        target(1200)
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 購入確認後に残高が変わったら支払不足と出品者の上限超過を再確認する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let now = purchase_time();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "funds", now)
        .await
        .unwrap();
    let not = NotRepository::new(db.clone());
    not.adjust_balance(admin(), buyer().user_id(), target(499), "spend")
        .await
        .unwrap();
    assert_eq!(
        shop.purchase(buyer(), &quote, now).await,
        Err(RepositoryError::InsufficientBalance)
    );
    not.adjust_balance(admin(), buyer().user_id(), target(1200), "restore")
        .await
        .unwrap();
    not.adjust_balance(admin(), user(), target(i64::MAX), "seller-max")
        .await
        .unwrap();
    assert_eq!(
        shop.purchase(buyer(), &quote, now).await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(i64::MAX));
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 複数の購入確認を同時に確定しても二重購入と残高の使い過ぎを防ぐ() {
    for mode in 0..3 {
        let file = TestDatabase::new();
        let first_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let second_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let (first, item) = purchase_setup(&file, &first_db, 700).await;
        let second = ShopRepository::new(second_db.clone());
        let other_item = if mode == 2 {
            first
                .create(
                    admin(),
                    details("別の商品", 500),
                    saved_content(&file, admin(), "other"),
                )
                .await
                .unwrap()
        } else {
            item.clone()
        };
        let now = purchase_time();
        let first_quote = first
            .prepare_purchase(buyer(), item.id(), "left", now)
            .await
            .unwrap();
        let second_quote = second
            .prepare_purchase(
                buyer(),
                other_item.id(),
                if mode == 0 { "left" } else { "right" },
                now,
            )
            .await
            .unwrap();
        let (left, right) = tokio::join!(
            first.purchase(buyer(), &first_quote, now),
            second.purchase(buyer(), &second_quote, now)
        );
        if mode == 0 {
            assert_eq!(left.unwrap(), right.unwrap());
        } else {
            let error = match (left, right) {
                (Ok(_), Err(error)) | (Err(error), Ok(_)) => error,
                other => panic!("expected one purchase: {other:?}"),
            };
            assert_eq!(
                error,
                if mode == 1 {
                    RepositoryError::AlreadyPurchased
                } else {
                    RepositoryError::InsufficientBalance
                }
            );
        }
        let raw = inspect(&file.path()).await;
        assert_eq!(trade_count(&raw).await, 2);
        let not = NotRepository::new(first_db.clone());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(200));
        assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
        raw.close().await.unwrap();
        first_db.close().await.unwrap();
        second_db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 売上の記録や確定に失敗したら購入と売上の両方を取り消す() {
    for fail_commit in [false, true] {
        let file = TestDatabase::new();
        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let (shop, item) = purchase_setup(&file, &db, 1200).await;
        let now = purchase_time();
        let quote = shop
            .prepare_purchase(buyer(), item.id(), "retry", now)
            .await
            .unwrap();
        let raw = inspect(&file.path()).await;
        let sql = if fail_commit {
            "CREATE TABLE commit_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_sale AFTER INSERT ON not_transaction WHEN NEW.reason = 'SALE'
             BEGIN INSERT INTO commit_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_sale BEFORE INSERT ON not_transaction WHEN NEW.reason = 'SALE'
             BEGIN SELECT RAISE(ABORT, 'injected sale failure'); END;"
        };
        raw.execute_unprepared(sql).await.unwrap();
        assert_eq!(
            shop.purchase(buyer(), &quote, now).await,
            Err(RepositoryError::Database(DatabaseError::InvalidData))
        );
        assert_eq!(trade_count(&raw).await, 0);
        let not = NotRepository::new(db.clone());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
        assert_eq!(not.get_balance(admin()).await.unwrap(), target(0));
        raw.execute_unprepared("DROP TRIGGER reject_sale")
            .await
            .unwrap();
        shop.purchase(buyer(), &quote, now).await.unwrap();
        assert_eq!(trade_count(&raw).await, 2);
        raw.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 返金後でも同じ購入確認を別の購入者や商品や金額には使えない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let not = NotRepository::new(db.clone());
    let other = domain::Actor::new(guild(), domain::UserId::new(202).unwrap(), false);
    not.adjust_balance(admin(), other.user_id(), target(1200), "seed-other")
        .await
        .unwrap();
    let other_item = shop
        .create(
            admin(),
            details("別の商品", 500),
            saved_content(&file, admin(), "other"),
        )
        .await
        .unwrap();
    let now = purchase_time();
    let original = shop
        .prepare_purchase(buyer(), item.id(), "same-id", now)
        .await
        .unwrap();
    let wrong_user = shop
        .prepare_purchase(other, item.id(), "same-id", now)
        .await
        .unwrap();
    let wrong_item = shop
        .prepare_purchase(buyer(), other_item.id(), "same-id", now)
        .await
        .unwrap();
    let result = shop.purchase(buyer(), &original, now).await.unwrap();
    assert_eq!(
        shop.purchase(other, &wrong_user, now).await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(
        shop.purchase(buyer(), &wrong_item, now).await,
        Err(RepositoryError::RequestConflict)
    );
    // 返金済み履歴のfixture。返金のユースケースはP06で実装する。
    let mut write = db.begin_write().await.unwrap();
    write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                buyer().user_id(),
                domain::NotAmount::new(500),
                LedgerReason::Refund(result.buyer().id()),
                "fixture:refund",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    write
        .append_entry(
            domain::NewLedgerEntry::new(
                guild(),
                user(),
                domain::NotAmount::new(-500),
                LedgerReason::SaleRefund(result.seller().unwrap().id()),
                "fixture:sale-refund",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !write
            .has_purchase(guild(), buyer().user_id(), item.id())
            .await
            .unwrap()
    );
    write.commit().await.unwrap();
    assert_eq!(
        shop.purchase(buyer(), &original, original.expires_at())
            .await
            .unwrap(),
        result
    );
    shop.update(admin(), item.id(), 1, details("変更価格", 600))
        .await
        .unwrap();
    let wrong_amount = shop
        .prepare_purchase(buyer(), item.id(), "same-id", now)
        .await
        .unwrap();
    assert_eq!(
        shop.purchase(buyer(), &wrong_amount, now).await,
        Err(RepositoryError::RequestConflict)
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 2);
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(0));
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 購入と売上の片側が欠けた履歴は不足分を追加せず拒否する() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, item) = purchase_setup(&file, &db, 1200).await;
    let now = purchase_time();
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "incomplete", now)
        .await
        .unwrap();
    let mut write = db.begin_write().await.unwrap();
    write
        .append_entry(entry(0, "content:incomplete:buyer"))
        .await
        .unwrap();
    write.commit().await.unwrap();
    assert_eq!(
        shop.purchase(buyer(), &quote, now).await,
        Err(RepositoryError::Database(DatabaseError::InvalidData))
    );
    let raw = inspect(&file.path()).await;
    assert_eq!(trade_count(&raw).await, 0);
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        2
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

async fn completed_purchase(
    file: &TestDatabase,
    db: &SqliteDatabaseAccess,
) -> (ShopRepository, PurchaseResult) {
    let (shop, item) = purchase_setup(file, db, 1200).await;
    let quote = shop
        .prepare_purchase(buyer(), item.id(), "paid", purchase_time())
        .await
        .unwrap();
    let result = shop
        .purchase(buyer(), &quote, purchase_time())
        .await
        .unwrap();
    (shop, result)
}

async fn refund_count(raw: &DatabaseConnection) -> i64 {
    number(
        raw,
        "SELECT COUNT(*) FROM not_transaction WHERE reason IN ('REFUND','SALE_REFUND')",
    )
    .await
}

#[tokio::test]
async fn 商品変更や削除と再起動の後も未返金の購入を復元し追加課金せず取得できる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, paid) = completed_purchase(&file, &db).await;
    let original_file = shop
        .get_content_for_purchase(buyer(), paid.buyer().id())
        .await
        .unwrap();
    shop.update(admin(), paid.item().id(), 1, details("価格変更", 900))
        .await
        .unwrap();
    shop.delete(admin(), paid.item().id(), 2).await.unwrap();
    db.close().await.unwrap();

    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let raw = inspect(&file.path()).await;
    let before = number(&raw, "SELECT COUNT(*) FROM not_transaction").await;
    let recovered = shop
        .find_purchase_by_confirmation(buyer(), "paid")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.buyer(), paid.buyer());
    assert_eq!(recovered.seller(), paid.seller());
    assert!(recovered.item().is_deleted());
    assert_eq!(recovered.item().details().price().get(), 900);
    assert_eq!(recovered.buyer().amount().get(), -500);
    assert_eq!(
        shop.find_purchase_by_confirmation(buyer(), "unknown")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        shop.list_purchases(buyer()).await.unwrap(),
        std::slice::from_ref(&recovered)
    );
    for _ in 0..2 {
        assert_eq!(
            shop.get_content_for_purchase(buyer(), paid.buyer().id())
                .await
                .unwrap(),
            original_file
        );
    }
    assert_eq!(
        fs::read(file.0.join("contents").join(original_file.key())).unwrap(),
        b"file contents"
    );
    assert_eq!(
        shop.find_purchase_by_confirmation(admin(), "paid").await,
        Err(RepositoryError::PermissionDenied)
    );
    assert_eq!(
        shop.get_content_for_purchase(admin(), paid.buyer().id())
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    assert!(shop.list_purchases(admin()).await.unwrap().is_empty());
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        before
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 返金は元の購入額で行い再送権限を取り消して再試行でも重複返金しない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, paid) = completed_purchase(&file, &db).await;
    shop.update(admin(), paid.item().id(), 1, details("値上げ", 900))
        .await
        .unwrap();
    shop.delete(admin(), paid.item().id(), 2).await.unwrap();
    let refund = shop
        .refund(admin(), paid.buyer().id(), "return")
        .await
        .unwrap();
    assert_eq!(
        refund.buyer().reason(),
        LedgerReason::Refund(paid.buyer().id())
    );
    assert_eq!(
        refund.seller().unwrap().reason(),
        LedgerReason::SaleRefund(paid.seller().unwrap().id())
    );
    assert_eq!(refund.buyer().amount().get(), 500);
    assert_eq!(refund.seller().unwrap().amount().get(), -500);
    assert_eq!(refund.buyer().event_key(), "refund:return:buyer");
    assert_eq!(refund.seller().unwrap().event_key(), "refund:return:seller");
    let not = NotRepository::new(db.clone());
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(0));
    assert_eq!(
        shop.get_content_for_purchase(buyer(), paid.buyer().id())
            .await,
        Err(RepositoryError::AlreadyRefunded)
    );
    assert!(shop.list_purchases(buyer()).await.unwrap().is_empty());
    assert_eq!(
        shop.refund(admin(), paid.buyer().id(), "new-id").await,
        Err(RepositoryError::AlreadyRefunded)
    );
    // 確定済みの再試行では現在の残高を使って再反映しない。
    not.adjust_balance(admin(), buyer().user_id(), target(0), "spent-after-refund")
        .await
        .unwrap();
    db.close().await.unwrap();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let shop = ShopRepository::new(db.clone());
    let raw = inspect(&file.path()).await;
    let before = number(&raw, "SELECT COUNT(*) FROM not_transaction").await;
    assert_eq!(
        shop.refund(admin(), paid.buyer().id(), "return")
            .await
            .unwrap(),
        refund
    );
    let recovered = shop
        .find_purchase_by_confirmation(buyer(), "paid")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.buyer(), paid.buyer());
    assert_eq!(recovered.seller(), paid.seller());
    assert_eq!(
        shop.get_content_for_purchase(buyer(), paid.buyer().id())
            .await,
        Err(RepositoryError::AlreadyRefunded)
    );
    assert_eq!(
        NotRepository::new(db.clone())
            .get_balance(buyer())
            .await
            .unwrap(),
        target(0)
    );
    assert_eq!(refund_count(&raw).await, 2);
    assert_eq!(
        number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
        before
    );
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 返金は管理者だけが元の購入と同じ依頼内容で実行できる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, paid) = completed_purchase(&file, &db).await;
    for actor in [buyer(), domain::Actor::new(guild(), user(), false)] {
        assert_eq!(
            shop.refund(actor, paid.buyer().id(), "return").await,
            Err(RepositoryError::PermissionDenied)
        );
    }
    for key in ["", " ", "bad\nkey"] {
        assert_eq!(
            shop.refund(admin(), paid.buyer().id(), key).await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidEventKey
            ))
        );
        assert_eq!(
            shop.find_purchase_by_confirmation(buyer(), key).await,
            Err(RepositoryError::Validation(
                ValidationError::InvalidEventKey
            ))
        );
    }
    for id in [
        domain::LedgerEntryId::new(1).unwrap(),
        domain::LedgerEntryId::new(999).unwrap(),
    ] {
        assert_eq!(
            shop.refund(admin(), id, "source").await,
            Err(RepositoryError::PurchaseNotFound)
        );
        assert_eq!(
            shop.get_content_for_purchase(buyer(), id).await,
            Err(RepositoryError::PurchaseNotFound)
        );
    }
    let operator = domain::Actor::new(guild(), domain::UserId::new(300).unwrap(), true);
    let returned = shop
        .refund(operator, paid.buyer().id(), "return")
        .await
        .unwrap();
    assert_eq!(returned.buyer().user_id(), buyer().user_id());
    assert_eq!(returned.seller().unwrap().user_id(), user());
    assert_eq!(
        shop.refund(buyer(), paid.buyer().id(), "return").await,
        Err(RepositoryError::PermissionDenied)
    );
    let next_quote = shop
        .prepare_purchase(
            buyer(),
            paid.item().id(),
            "second-purchase",
            purchase_time(),
        )
        .await
        .unwrap();
    let next = shop
        .purchase(buyer(), &next_quote, purchase_time())
        .await
        .unwrap();
    assert_eq!(
        shop.refund(operator, next.buyer().id(), "return").await,
        Err(RepositoryError::RequestConflict)
    );
    assert_eq!(
        shop.list_purchases(buyer()).await.unwrap(),
        std::slice::from_ref(&next)
    );
    let recovered = shop
        .find_purchase_by_confirmation(buyer(), "paid")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.buyer(), paid.buyer());
    let raw = inspect(&file.path()).await;
    assert_eq!(refund_count(&raw).await, 2);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 同じ利用者と確認番号でも購入一覧と返金はサーバーごとに分かれる() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, paid) = completed_purchase(&file, &db).await;
    let other_admin = domain::Actor::new(domain::GuildId::new(101).unwrap(), user(), true);
    let other_buyer = domain::Actor::new(other_admin.guild_id(), buyer().user_id(), false);
    NotRepository::new(db.clone())
        .adjust_balance(other_admin, other_buyer.user_id(), target(1000), "seed")
        .await
        .unwrap();
    let other_item = shop
        .create(
            other_admin,
            details("別ギルド", 500),
            saved_content(&file, other_admin, "other"),
        )
        .await
        .unwrap();
    let quote = shop
        .prepare_purchase(other_buyer, other_item.id(), "paid", purchase_time())
        .await
        .unwrap();
    let other_paid = shop
        .purchase(other_buyer, &quote, purchase_time())
        .await
        .unwrap();
    assert_eq!(
        shop.list_purchases(buyer()).await.unwrap(),
        std::slice::from_ref(&paid)
    );
    assert_eq!(
        shop.list_purchases(other_buyer).await.unwrap(),
        std::slice::from_ref(&other_paid)
    );
    assert_eq!(
        shop.find_purchase_by_confirmation(other_buyer, "paid")
            .await
            .unwrap(),
        Some(other_paid)
    );
    assert_eq!(
        shop.get_content_for_purchase(other_buyer, paid.buyer().id())
            .await,
        Err(RepositoryError::PurchaseNotFound)
    );
    assert_eq!(
        shop.refund(other_admin, paid.buyer().id(), "cross-guild")
            .await,
        Err(RepositoryError::PurchaseNotFound)
    );
    let missing = domain::Actor::new(domain::GuildId::new(102).unwrap(), buyer().user_id(), false);
    assert_eq!(
        shop.find_purchase_by_confirmation(missing, "paid")
            .await
            .unwrap(),
        None
    );
    assert!(shop.list_purchases(missing).await.unwrap().is_empty());
    let raw = inspect(&file.path()).await;
    assert_eq!(refund_count(&raw).await, 0);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 返金できる残高がなければ両者の残高と購入者の取得権限を変えない() {
    let file = TestDatabase::new();
    let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
    let (shop, paid) = completed_purchase(&file, &db).await;
    let not = NotRepository::new(db.clone());
    not.adjust_balance(admin(), user(), target(499), "seller-spent")
        .await
        .unwrap();
    assert_eq!(
        shop.refund(admin(), paid.buyer().id(), "return").await,
        Err(RepositoryError::InsufficientBalance)
    );
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(700));
    not.adjust_balance(admin(), user(), target(500), "restore-seller")
        .await
        .unwrap();
    not.adjust_balance(admin(), buyer().user_id(), target(i64::MAX), "buyer-max")
        .await
        .unwrap();
    assert_eq!(
        shop.refund(admin(), paid.buyer().id(), "return").await,
        Err(RepositoryError::Money(MoneyError::Overflow))
    );
    assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
    assert_eq!(not.get_balance(buyer()).await.unwrap(), target(i64::MAX));
    shop.get_content_for_purchase(buyer(), paid.buyer().id())
        .await
        .unwrap();
    let raw = inspect(&file.path()).await;
    assert_eq!(refund_count(&raw).await, 0);
    raw.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn 同時の返金や新しい購入との競合でも同じ購入を二重返金しない() {
    for mode in 0..3 {
        let file = TestDatabase::new();
        let first_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let second_db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let (first, paid) = completed_purchase(&file, &first_db).await;
        let second = ShopRepository::new(second_db.clone());
        if mode < 2 {
            let (left, right) = tokio::join!(
                first.refund(admin(), paid.buyer().id(), "left"),
                second.refund(
                    admin(),
                    paid.buyer().id(),
                    if mode == 0 { "left" } else { "right" }
                )
            );
            if mode == 0 {
                assert_eq!(left.unwrap(), right.unwrap());
            } else {
                assert!(matches!(
                    (left, right),
                    (Ok(_), Err(RepositoryError::AlreadyRefunded))
                        | (Err(RepositoryError::AlreadyRefunded), Ok(_))
                ));
            }
            assert_eq!(
                NotRepository::new(first_db.clone())
                    .get_balance(buyer())
                    .await
                    .unwrap(),
                target(1200)
            );
            assert_eq!(
                NotRepository::new(first_db.clone())
                    .get_balance(admin())
                    .await
                    .unwrap(),
                target(0)
            );
        } else {
            // 返金と、別ユーザーの購入を同じ出品者残高に対して競合させる。
            let other = domain::Actor::new(guild(), domain::UserId::new(202).unwrap(), false);
            NotRepository::new(first_db.clone())
                .adjust_balance(admin(), other.user_id(), target(500), "seed-other")
                .await
                .unwrap();
            let quote = second
                .prepare_purchase(other, paid.item().id(), "other", purchase_time())
                .await
                .unwrap();
            let (returned, bought) = tokio::join!(
                first.refund(admin(), paid.buyer().id(), "left"),
                second.purchase(other, &quote, purchase_time())
            );
            returned.unwrap();
            bought.unwrap();
            let not = NotRepository::new(first_db.clone());
            assert_eq!(not.get_balance(buyer()).await.unwrap(), target(1200));
            assert_eq!(not.get_balance(other).await.unwrap(), target(0));
            assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
        }
        assert_eq!(
            first
                .get_content_for_purchase(buyer(), paid.buyer().id())
                .await,
            Err(RepositoryError::AlreadyRefunded)
        );
        let raw = inspect(&file.path()).await;
        assert_eq!(refund_count(&raw).await, 2);
        raw.close().await.unwrap();
        first_db.close().await.unwrap();
        second_db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 返金の途中や確定に失敗したら返金と売上取消の両方を取り消す() {
    for fail_commit in [false, true] {
        let file = TestDatabase::new();
        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let (shop, paid) = completed_purchase(&file, &db).await;
        let raw = inspect(&file.path()).await;
        let sql = if fail_commit {
            "CREATE TABLE commit_guard (code TEXT REFERENCES transaction_reason(code) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER reject_refund AFTER INSERT ON not_transaction WHEN NEW.reason = 'REFUND'
             BEGIN INSERT INTO commit_guard VALUES ('UNKNOWN'); END;"
        } else {
            "CREATE TRIGGER reject_refund BEFORE INSERT ON not_transaction WHEN NEW.reason = 'REFUND'
             BEGIN SELECT RAISE(ABORT, 'injected refund failure'); END;"
        };
        raw.execute_unprepared(sql).await.unwrap();
        assert_eq!(
            shop.refund(admin(), paid.buyer().id(), "retry").await,
            Err(RepositoryError::Database(DatabaseError::InvalidData))
        );
        let not = NotRepository::new(db.clone());
        assert_eq!(not.get_balance(buyer()).await.unwrap(), target(700));
        assert_eq!(not.get_balance(admin()).await.unwrap(), target(500));
        shop.get_content_for_purchase(buyer(), paid.buyer().id())
            .await
            .unwrap();
        assert_eq!(refund_count(&raw).await, 0);
        raw.execute_unprepared("DROP TRIGGER reject_refund")
            .await
            .unwrap();
        shop.refund(admin(), paid.buyer().id(), "retry")
            .await
            .unwrap();
        assert_eq!(refund_count(&raw).await, 2);
        raw.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test]
async fn 売買や返金の履歴が不整合なら結果復元と取得と返金を拒否する() {
    for mode in 0..5 {
        let file = TestDatabase::new();
        let db = SqliteDatabaseAccess::open(file.path()).await.unwrap();
        let (shop, item) = purchase_setup(&file, &db, 1200).await;
        let mut write = db.begin_write().await.unwrap();
        let purchase = write
            .append_entry(
                domain::NewLedgerEntry::new(
                    guild(),
                    buyer().user_id(),
                    domain::NotAmount::new(-500),
                    LedgerReason::Purchase(item.id()),
                    "content:broken:buyer",
                )
                .unwrap(),
            )
            .await
            .unwrap();
        if mode != 0 {
            let seller = write
                .append_entry(
                    domain::NewLedgerEntry::new(
                        guild(),
                        if mode == 2 {
                            domain::UserId::new(202).unwrap()
                        } else {
                            user()
                        },
                        domain::NotAmount::new(if mode == 1 { 600 } else { 500 }),
                        LedgerReason::Sale(purchase.id()),
                        "content:broken:seller",
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            if mode >= 3 {
                write
                    .append_entry(
                        domain::NewLedgerEntry::new(
                            guild(),
                            buyer().user_id(),
                            domain::NotAmount::new(if mode == 4 { 499 } else { 500 }),
                            LedgerReason::Refund(purchase.id()),
                            "refund:broken:buyer",
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                if mode == 4 {
                    write
                        .append_entry(
                            domain::NewLedgerEntry::new(
                                guild(),
                                user(),
                                domain::NotAmount::new(-500),
                                LedgerReason::SaleRefund(seller.id()),
                                "refund:broken:seller",
                            )
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                }
            }
        }
        write.commit().await.unwrap();
        let raw = inspect(&file.path()).await;
        let before = number(&raw, "SELECT COUNT(*) FROM not_transaction").await;
        let invalid = Err(RepositoryError::Database(DatabaseError::InvalidData));
        assert_eq!(shop.list_purchases(buyer()).await, invalid);
        assert_eq!(
            shop.get_content_for_purchase(buyer(), purchase.id()).await,
            Err(RepositoryError::Database(DatabaseError::InvalidData))
        );
        assert_eq!(
            shop.refund(admin(), purchase.id(), "new-refund").await,
            Err(RepositoryError::Database(DatabaseError::InvalidData))
        );
        // 領収結果の復元は返金権限判定を行わない。壊れた購入・売上は拒否する。
        if mode < 3 {
            assert_eq!(
                shop.find_purchase_by_confirmation(buyer(), "broken").await,
                Err(RepositoryError::Database(DatabaseError::InvalidData))
            );
        }
        assert_eq!(
            number(&raw, "SELECT COUNT(*) FROM not_transaction").await,
            before
        );
        raw.close().await.unwrap();
        db.close().await.unwrap();
    }
}
