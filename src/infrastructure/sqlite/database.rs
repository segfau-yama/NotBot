use crate::domain;
use crate::domain::*;
use crate::infrastructure::sqlite::not::NotRepository as SqliteNotRepository;
use crate::infrastructure::sqlite::shop::ShopRepository as SqliteShopRepository;
use crate::infrastructure::types as values;
use crate::infrastructure::{MoneyError, ValidationError};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, RuntimeErr,
    SqliteTransactionMode, SqlxSqliteConnector, Statement, TransactionOptions, TransactionTrait,
    raw_sql,
    sqlx::{
        self,
        sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    },
};
use std::{fs, path::Path, time::Duration};

pub use crate::infrastructure::types::DatabaseError;

pub(super) fn map_commit_error(error: DbErr) -> DatabaseError {
    match DatabaseError::from(error) {
        DatabaseError::Unavailable => DatabaseError::OutcomeUnknown,
        error => error,
    }
}

impl From<DbErr> for DatabaseError {
    fn from(error: DbErr) -> Self {
        eprintln!("SQLite: {error}");
        match error {
            DbErr::Exec(RuntimeErr::SqlxError(error))
            | DbErr::Query(RuntimeErr::SqlxError(error))
            | DbErr::Conn(RuntimeErr::SqlxError(error)) => match error.as_ref() {
                sqlx::Error::Database(error) => {
                    let code = error.code().and_then(|code| code.parse::<i32>().ok());
                    match code {
                        Some(1555 | 2067) => DatabaseError::Conflict,
                        Some(code) => match code & 0xff {
                            5 | 6 => DatabaseError::Conflict,
                            8 => DatabaseError::ReadOnly,
                            1 | 11 | 18 | 19 | 20 | 26 => DatabaseError::InvalidData,
                            _ => DatabaseError::Unavailable,
                        },
                        None => DatabaseError::Unavailable,
                    }
                }
                sqlx::Error::ColumnDecode { .. }
                | sqlx::Error::Decode(_)
                | sqlx::Error::ColumnNotFound(_)
                | sqlx::Error::RowNotFound => DatabaseError::InvalidData,
                _ => DatabaseError::Unavailable,
            },
            DbErr::Type(_)
            | DbErr::TryIntoErr { .. }
            | DbErr::ConvertFromU64(_)
            | DbErr::UnpackInsertId => DatabaseError::InvalidData,
            _ => DatabaseError::Unavailable,
        }
    }
}

impl From<ValidationError> for DatabaseError {
    fn from(_: ValidationError) -> Self {
        Self::InvalidData
    }
}

impl From<MoneyError> for DatabaseError {
    fn from(_: MoneyError) -> Self {
        Self::InvalidData
    }
}

#[derive(Clone)]
pub struct SqliteDatabaseAccess {
    connection: DatabaseConnection,
}

impl SqliteDatabaseAccess {
    /// DBファイルと親ディレクトリを作成し、必要なマイグレーションを適用する。
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err(DatabaseError::InvalidData);
        }
        let path = std::path::absolute(path).map_err(|error| {
            eprintln!("SQLite path: {error}");
            DatabaseError::Unavailable
        })?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                eprintln!("SQLite directory: {error}");
                DatabaseError::Unavailable
            })?;
        }
        // URL文字列を組み立てず、空白や ? # を含むパスもそのまま渡す。
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(|error| {
                DatabaseError::from(DbErr::Conn(RuntimeErr::SqlxError(error.into())))
            })?;
        let database = Self {
            connection: SqlxSqliteConnector::from_sqlx_sqlite_pool(pool),
        };
        database.migrate().await?;
        Ok(database)
    }

    pub async fn close(self) -> Result<(), DatabaseError> {
        self.connection.close().await.map_err(Into::into)
    }

    async fn migrate(&self) -> Result<(), DatabaseError> {
        let transaction = self.connection.begin_with_options(write_options()).await?;
        let row = transaction
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA user_version",
            ))
            .await?
            .ok_or(DatabaseError::InvalidData)?;
        let version: i64 = row.try_get("", "user_version")?;
        if !(0..=2).contains(&version) {
            return Err(DatabaseError::InvalidData);
        }
        if version == 0 {
            transaction
                .execute_unprepared(include_str!("001_initial.sql"))
                .await?;
        }
        if version < 2 {
            transaction
                .execute_unprepared(include_str!("002_rewards.sql"))
                .await?;
            transaction
                .execute_unprepared("PRAGMA user_version = 2")
                .await?;
        }
        transaction.commit().await.map_err(map_commit_error)
    }
}

fn write_options() -> TransactionOptions {
    TransactionOptions {
        sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
        ..Default::default()
    }
}

impl SqliteDatabaseAccess {
    pub async fn begin_read(&self) -> Result<SqliteSession, DatabaseError> {
        let transaction = self.connection.begin().await?;
        Ok(SqliteSession::new(transaction, false))
    }

    pub async fn begin_write(&self) -> Result<SqliteSession, DatabaseError> {
        let transaction = self.connection.begin_with_options(write_options()).await?;
        Ok(SqliteSession::new(transaction, true))
    }
}

pub struct SqliteSession {
    transaction: Option<DatabaseTransaction>,
    pub(super) writable: bool,
    failure: Option<DatabaseError>,
}

impl SqliteSession {
    pub(super) fn new(transaction: DatabaseTransaction, writable: bool) -> Self {
        Self {
            transaction: Some(transaction),
            writable,
            failure: None,
        }
    }

    pub(super) fn take_transaction(&mut self) -> Result<DatabaseTransaction, DatabaseError> {
        self.transaction
            .take()
            .ok_or(self.failure.unwrap_or(DatabaseError::Unavailable))
    }

    pub(super) async fn finish_operation<T: Send>(
        &mut self,
        transaction: DatabaseTransaction,
        result: Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        match result {
            Ok(value) => {
                self.transaction = Some(transaction);
                Ok(value)
            }
            Err(error) => {
                self.failure = Some(error);
                transaction.rollback().await?;
                Err(error)
            }
        }
    }

    pub async fn commit(mut self) -> Result<(), DatabaseError> {
        self.take_transaction()?
            .commit()
            .await
            .map_err(map_commit_error)
    }

    pub async fn rollback(mut self) -> Result<(), DatabaseError> {
        self.take_transaction()?
            .rollback()
            .await
            .map_err(Into::into)
    }
}

impl SqliteSession {
    async fn query_entry(
        &mut self,
        query: Statement,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction
                .query_one_raw(query)
                .await?
                .map(SqliteNotRepository::read_entry)
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_entry(
        &mut self,
        guild_id: GuildId,
        entry_id: LedgerEntryId,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let guild = guild_id.to_string();
        let id = entry_id.get();
        self.query_entry(raw_sql!(
            Sqlite,
            "SELECT * FROM not_transaction WHERE guild_id = {guild} AND id = {id}"
        ))
        .await
    }

    pub async fn find_sale(
        &mut self,
        guild_id: GuildId,
        purchase_id: LedgerEntryId,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let guild = guild_id.to_string();
        let id = purchase_id.get();
        self.query_entry(raw_sql!(
            Sqlite,
            "SELECT * FROM not_transaction WHERE guild_id = {guild} AND sale_of = {id}"
        ))
        .await
    }

    pub async fn find_refund(
        &mut self,
        guild_id: GuildId,
        source_id: LedgerEntryId,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let guild = guild_id.to_string();
        let id = source_id.get();
        self.query_entry(raw_sql!(
            Sqlite,
            "SELECT * FROM not_transaction WHERE guild_id = {guild} AND refund_of = {id}"
        ))
        .await
    }

    pub async fn list_purchase_entries(
        &mut self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Vec<LedgerEntry>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = guild_id.to_string();
        let user = user_id.to_string();
        let result = async {
            transaction.query_all_raw(raw_sql!(Sqlite,
                "SELECT * FROM not_transaction
                 WHERE guild_id = {guild} AND user_id = {user} AND reason = 'PURCHASE' ORDER BY id DESC"
            )).await?.into_iter().map(SqliteNotRepository::read_entry).collect()
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub(super) async fn list_role_purchase_entries(
        &mut self,
        guild_id: GuildId,
    ) -> Result<Vec<LedgerEntry>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = guild_id.to_string();
        let result = async {
            transaction.query_all_raw(raw_sql!(Sqlite,
                "SELECT p.* FROM not_transaction p JOIN shop_item i ON i.id = p.item_id
                 WHERE p.guild_id = {guild} AND p.reason = 'PURCHASE' AND i.kind = 'ROLE' ORDER BY p.id"
            )).await?.into_iter().map(SqliteNotRepository::read_entry).collect()
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn has_purchase(
        &mut self,
        guild_id: GuildId,
        user_id: UserId,
        item_id: ItemId,
    ) -> Result<bool, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = guild_id.to_string();
        let user = user_id.to_string();
        let item = item_id.get();
        let result = async {
            let row = transaction
                .query_one_raw(raw_sql!(
                    Sqlite,
                    "SELECT EXISTS (
                    SELECT 1 FROM not_transaction AS p
                    WHERE p.guild_id = {guild} AND p.user_id = {user}
                        AND p.item_id = {item} AND p.reason = 'PURCHASE'
                        AND NOT EXISTS (
                            SELECT 1 FROM not_transaction AS r
                            WHERE r.refund_of = p.id AND r.reason = 'REFUND'
                                AND r.guild_id = p.guild_id AND r.user_id = p.user_id
                        )
                 ) AS purchased"
                ))
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            let purchased: bool = row.try_get("", "purchased")?;
            Ok(purchased)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn get_balance(
        &mut self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Balance, DatabaseError> {
        // await中はFutureがトランザクションを所有する。キャンセル時はSeaORMのDropが
        // ROLLBACKをキューへ送り、このセッションから未確定の続きを確定できなくする。
        let transaction = self.take_transaction()?;
        let result = async {
            let row = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "SELECT COALESCE((
                        SELECT balance FROM not_balance WHERE guild_id = ? AND user_id = ?
                    ), 0) AS balance",
                    [guild_id.to_string().into(), user_id.to_string().into()],
                ))
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            let value: i64 = row.try_get("", "balance")?;
            domain::Balance::new(value).map_err(Into::into)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn get_ranking(
        &mut self,
        guild_id: GuildId,
        limit: u32,
    ) -> Result<Vec<UserBalance>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            let rows = transaction
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "SELECT user_id, balance FROM not_balance WHERE guild_id = ?
                     ORDER BY balance DESC, user_id ASC LIMIT ?",
                    [guild_id.to_string().into(), i64::from(limit).into()],
                ))
                .await?;
            rows.into_iter()
                .map(|row| {
                    let user: String = row.try_get("", "user_id")?;
                    let balance: i64 = row.try_get("", "balance")?;
                    Ok(UserBalance::new(
                        values::parse_user_id(&user)?,
                        domain::Balance::new(balance)?,
                    ))
                })
                .collect()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_by_event_key(
        &mut self,
        guild_id: GuildId,
        event_key: &str,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let guild = guild_id.to_string();
        self.query_entry(raw_sql!(
            Sqlite,
            "SELECT * FROM not_transaction WHERE guild_id = {guild} AND event_key = {event_key}"
        ))
        .await
    }

    pub async fn get_balance_at_entry(
        &mut self,
        guild_id: GuildId,
        user_id: UserId,
        entry_id: LedgerEntryId,
    ) -> Result<Balance, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            let row = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "SELECT SUM(amount) AS balance FROM (
                        SELECT amount FROM not_transaction
                        WHERE guild_id = ? AND user_id = ? AND id <= ? ORDER BY id
                     )",
                    [
                        guild_id.to_string().into(),
                        user_id.to_string().into(),
                        entry_id.get().into(),
                    ],
                ))
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            let balance: i64 = row.try_get("", "balance")?;
            domain::Balance::new(balance).map_err(Into::into)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_entry(
        &mut self,
        entry: NewLedgerEntry,
    ) -> Result<LedgerEntry, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let (item_id, sale_of, refund_of): (Option<i64>, Option<i64>, Option<i64>) = match entry
            .reason()
        {
            LedgerReason::Purchase(id) => (Some(id.get()), None, None),
            LedgerReason::Sale(id) => (None, Some(id.get()), None),
            LedgerReason::Refund(id) | LedgerReason::SaleRefund(id) => (None, None, Some(id.get())),
            _ => (None, None, None),
        };
        let result = async {
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "INSERT INTO not_transaction
                        (guild_id, user_id, amount, reason, event_key, item_id, sale_of, refund_of)
                     SELECT ?, ?, ?, ?, ?, ?, ?, ?
                     WHERE NOT EXISTS (SELECT 1 FROM not_transaction WHERE id = ?)",
                    [
                        entry.guild_id().to_string().into(),
                        entry.user_id().to_string().into(),
                        entry.amount().get().into(),
                        values::reason_code(entry.reason()).into(),
                        entry.event_key().into(),
                        item_id.into(),
                        sale_of.into(),
                        refund_of.into(),
                        i64::MAX.into(),
                    ],
                ))
                .await?;
            // ID上限でSQLiteが小さいランダムIDを再利用する動作を止める。
            // INSERTのみの履歴では、ID順と追記順が一致することを保証する。
            if inserted.rows_affected() != 1 {
                return Err(DatabaseError::InvalidData);
            }
            let id = i64::try_from(inserted.last_insert_id())
                .ok()
                .and_then(|id| domain::LedgerEntryId::new(id).ok())
                .ok_or(DatabaseError::InvalidData)?;
            domain::LedgerEntry::new(
                id,
                entry.guild_id(),
                entry.user_id(),
                entry.amount(),
                entry.reason(),
                entry.event_key(),
            )
            .map_err(Into::into)
        }
        .await;
        self.finish_operation(transaction, result).await
    }
}

const CURRENT_ITEM_QUERY: &str = "SELECT i.*, f.key AS file_key, f.filename, f.size_bytes
    FROM current_shop_item AS i
    LEFT JOIN content_file AS f ON i.kind = 'CONTENT' AND f.key = i.target";

impl SqliteSession {
    async fn query_item(&mut self, query: Statement) -> Result<Option<ShopItem>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction
                .query_one_raw(query)
                .await?
                .map(SqliteShopRepository::read_item)
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_item(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!("{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.id = ?"),
            [guild_id.to_string().into(), item_id.get().into()],
        ))
        .await
    }

    pub async fn find_content_item(
        &mut self,
        guild_id: GuildId,
        key: &str,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!(
                "{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.kind = 'CONTENT' AND i.target = ?"
            ),
            [guild_id.to_string().into(), key.into()],
        ))
        .await
    }

    pub(super) async fn find_role_item(
        &mut self,
        guild: GuildId,
        role: RoleId,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!(
                "{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.kind = 'ROLE' AND i.target = ?"
            ),
            [guild.to_string().into(), role.to_string().into()],
        ))
        .await
    }

    pub(super) async fn append_role_item(
        &mut self,
        actor: Actor,
        role: RoleId,
        details: ItemDetails,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let seller = actor.user_id().to_string();
        let target = role.to_string();
        let result = async {
            let inserted = transaction.execute_raw(raw_sql!(Sqlite,
                "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES ({guild}, {seller}, 'ROLE', {target})"
            )).await?;
            let id = i64::try_from(inserted.last_insert_id()).map_err(|_| DatabaseError::InvalidData)?;
            let item = domain::ShopItem::new(domain::ItemId::new(id)?, actor.guild_id(), actor.user_id(), ItemTarget::Role(role), details, 1, false)?;
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_item_revision(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
        revision: i64,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT i.id, i.guild_id, i.seller_id, i.kind, i.target,
                    r.revision, r.name, r.description, r.price, r.deleted_at,
                    f.key AS file_key, f.filename, f.size_bytes
             FROM shop_item AS i JOIN shop_item_revision AS r ON r.item_id = i.id
             LEFT JOIN content_file AS f ON i.kind = 'CONTENT' AND f.key = i.target
             WHERE i.guild_id = ? AND i.id = ? AND r.revision = ?",
            [
                guild_id.to_string().into(),
                item_id.get().into(),
                revision.into(),
            ],
        ))
        .await
    }

    pub async fn list_active_items(
        &mut self,
        guild_id: GuildId,
    ) -> Result<Vec<ShopItem>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction.query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.deleted_at IS NULL ORDER BY i.id"),
                [guild_id.to_string().into()],
            )).await?.into_iter().map(SqliteShopRepository::read_item).collect()
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_content_item(
        &mut self,
        actor: Actor,
        item: NewShopItem,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let result = async {
            let ItemTarget::Content(file) = item.target() else {
                return Err(DatabaseError::InvalidData);
            };
            transaction.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO content_file (key, filename, size_bytes) VALUES (?, ?, ?)",
                [file.key().into(), file.filename().into(), (file.size_bytes() as i64).into()],
            )).await?;
            let inserted = transaction.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES (?, ?, 'CONTENT', ?)",
                [actor.guild_id().to_string().into(), actor.user_id().to_string().into(), file.key().into()],
            )).await?;
            let id = i64::try_from(inserted.last_insert_id()).ok()
                .and_then(|id| domain::ItemId::new(id).ok()).ok_or(DatabaseError::InvalidData)?;
            let item = domain::ShopItem::new(id, actor.guild_id(), actor.user_id(), item.target().clone(), item.details().clone(), 1, false)?;
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_item_revision(
        &mut self,
        item: ShopItem,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let result = async {
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_publication(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
    ) -> Result<Option<(i64, ChannelId, MessageId)>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = guild_id.to_string();
        let item = item_id.get();
        let result = async {
            transaction
                .query_one_raw(raw_sql!(
                    Sqlite,
                    "SELECT p.item_revision, p.channel_id, p.message_id
                 FROM shop_item_publication AS p JOIN shop_item AS i ON i.id = p.item_id
                 WHERE i.guild_id = {guild} AND i.id = {item} ORDER BY p.id DESC LIMIT 1"
                ))
                .await?
                .map(|row| {
                    let revision: i64 = row.try_get("", "item_revision")?;
                    let channel: String = row.try_get("", "channel_id")?;
                    let message: String = row.try_get("", "message_id")?;
                    if revision <= 0 {
                        return Err(DatabaseError::InvalidData);
                    }
                    Ok((
                        revision,
                        values::parse_channel_id(&channel)?,
                        values::parse_message_id(&message)?,
                    ))
                })
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_publication(
        &mut self,
        item: &ShopItem,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> Result<(), DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let item_id = item.id().get();
        let revision = item.revision();
        let channel = channel_id.to_string();
        let message = message_id.to_string();
        let result = async {
            transaction
                .execute_raw(raw_sql!(Sqlite,
                "INSERT INTO shop_item_publication (item_id, item_revision, channel_id, message_id)
                 VALUES ({item_id}, {revision}, {channel}, {message})"
            ))
                .await?;
            Ok(())
        }
        .await;
        self.finish_operation(transaction, result).await
    }
}

async fn insert_revision(
    transaction: &DatabaseTransaction,
    item: &ShopItem,
) -> Result<(), DatabaseError> {
    transaction.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO shop_item_revision (item_id, revision, name, description, price, deleted_at)
         VALUES (?, ?, ?, ?, ?, CASE WHEN ? THEN CURRENT_TIMESTAMP ELSE NULL END)",
        [item.id().get().into(), item.revision().into(), item.details().name().into(),
         item.details().description().into(), item.details().price().get().into(), item.is_deleted().into()],
    )).await?;
    Ok(())
}

impl SqliteSession {
    pub(super) async fn count_voice_rewards(
        &mut self,
        actor: Actor,
        prefix: &str,
    ) -> Result<i64, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let user = actor.user_id().to_string();
        let pattern = format!("{prefix}%");
        let result = async {
            transaction
                .query_one_raw(raw_sql!(
                    Sqlite,
                    "SELECT COUNT(*) AS total FROM not_transaction
                 WHERE guild_id = {guild} AND user_id = {user}
                   AND reason = 'ADJUSTMENT' AND amount = 1 AND event_key LIKE {pattern}"
                ))
                .await?
                .ok_or(DatabaseError::InvalidData)?
                .try_get("", "total")
                .map_err(Into::into)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub(super) async fn find_reward_event(
        &mut self,
        actor: Actor,
        key: &str,
    ) -> Result<Option<(UserId, String, Option<LedgerEntryId>)>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let result = async {
            transaction
                .query_one_raw(raw_sql!(
                    Sqlite,
                    "SELECT user_id, reason, entry_id FROM reward_event
                 WHERE guild_id = {guild} AND event_key = {key}"
                ))
                .await?
                .map(|row| {
                    let user: String = row.try_get("", "user_id")?;
                    let entry: Option<i64> = row.try_get("", "entry_id")?;
                    Ok((
                        values::parse_user_id(&user)?,
                        row.try_get("", "reason")?,
                        entry.map(domain::LedgerEntryId::new).transpose()?,
                    ))
                })
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub(super) async fn reward_allowed(
        &mut self,
        actor: Actor,
        reason: LedgerReason,
        now: i64,
    ) -> Result<bool, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let user = actor.user_id().to_string();
        let reason = values::reason_code(reason);
        let day_start = (now + 32_400_000) / 86_400_000 * 86_400_000 - 32_400_000;
        let day_end = day_start + 86_400_000;
        let result = async {
            let row = transaction.query_one_raw(raw_sql!(Sqlite,
                "SELECT MAX(received_at) AS latest,
                        COUNT(CASE WHEN received_at >= {day_start} AND received_at < {day_end} THEN 1 END) AS daily
                 FROM reward_event WHERE guild_id = {guild} AND user_id = {user}
                     AND reason = {reason} AND entry_id IS NOT NULL"
            )).await?.ok_or(DatabaseError::InvalidData)?;
            let latest: Option<i64> = row.try_get("", "latest")?;
            let daily: i64 = row.try_get("", "daily")?;
            Ok(daily < 100 && latest.is_none_or(|latest| latest <= now - 60_000))
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub(super) async fn append_reward_event(
        &mut self,
        actor: Actor,
        reason: LedgerReason,
        key: &str,
        now: i64,
        entry: Option<LedgerEntryId>,
    ) -> Result<(), DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let user = actor.user_id().to_string();
        let reason = values::reason_code(reason);
        let entry = entry.map(LedgerEntryId::get);
        let result = async {
            transaction.execute_raw(raw_sql!(Sqlite,
                "INSERT INTO reward_event (guild_id, event_key, user_id, reason, received_at, entry_id)
                 VALUES ({guild}, {key}, {user}, {reason}, {now}, {entry})"
            )).await?;
            Ok(())
        }.await;
        self.finish_operation(transaction, result).await
    }
}
