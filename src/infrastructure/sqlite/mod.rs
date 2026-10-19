pub mod not;
pub mod shop;

use std::{fs, path::Path, time::Duration};

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, QueryResult,
    RuntimeErr, SqliteTransactionMode, SqlxSqliteConnector, Statement, TransactionOptions,
    TransactionTrait, raw_sql,
    sqlx::{
        self,
        sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    },
};

use crate::domain::{
    Actor, Balance, ContentFile, GuildId, ItemDetails, ItemId, ItemTarget, LedgerEntry,
    LedgerEntryId, LedgerReason, MoneyError, NewLedgerEntry, NewShopItem, NotAmount, Price,
    ShopItem, UserBalance, UserId, ValidationError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DatabaseError {
    #[error("データベースを利用できません")]
    Unavailable,
    #[error("データの書き込みが競合しました")]
    Conflict,
    #[error("データの形式または整合性が不正です")]
    InvalidData,
    #[error("読み取り専用セッションには追記できません")]
    ReadOnly,
    #[error("確定結果を確認できません。同じ依頼IDで結果を照合してください")]
    OutcomeUnknown,
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
        match version {
            0 => {
                transaction
                    .execute_unprepared(include_str!("001_initial.sql"))
                    .await?;
                transaction
                    .execute_unprepared("PRAGMA user_version = 1")
                    .await?;
            }
            1 => {}
            _ => return Err(DatabaseError::InvalidData),
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
    writable: bool,
    failure: Option<DatabaseError>,
}

impl SqliteSession {
    fn new(transaction: DatabaseTransaction, writable: bool) -> Self {
        Self {
            transaction: Some(transaction),
            writable,
            failure: None,
        }
    }

    fn take_transaction(&mut self) -> Result<DatabaseTransaction, DatabaseError> {
        self.transaction
            .take()
            .ok_or(self.failure.unwrap_or(DatabaseError::Unavailable))
    }

    async fn finish_operation<T: Send>(
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

    async fn query_item(&mut self, query: Statement) -> Result<Option<ShopItem>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction
                .query_one_raw(query)
                .await?
                .map(read_item)
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    async fn query_entry(
        &mut self,
        query: Statement,
    ) -> Result<Option<LedgerEntry>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction
                .query_one_raw(query)
                .await?
                .map(read_entry)
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }
}

const CURRENT_ITEM_QUERY: &str = "SELECT i.*, f.key AS file_key, f.filename, f.size_bytes
    FROM current_shop_item AS i
    LEFT JOIN content_file AS f ON i.kind = 'CONTENT' AND f.key = i.target";

impl SqliteSession {
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
            )).await?.into_iter().map(read_entry).collect()
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
            )).await?.into_iter().map(read_item).collect()
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
                .and_then(|id| ItemId::try_from(id).ok()).ok_or(DatabaseError::InvalidData)?;
            let item = ShopItem::try_new(id, actor.guild_id(), actor.user_id(), item.target().clone(), item.details().clone(), 1, false)?;
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
            Balance::try_from(value).map_err(Into::into)
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
                    Ok(UserBalance::new(user.parse()?, Balance::try_from(balance)?))
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
            Balance::try_from(balance).map_err(Into::into)
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
                        entry.reason().code().into(),
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
                .and_then(|id| LedgerEntryId::try_from(id).ok())
                .ok_or(DatabaseError::InvalidData)?;
            LedgerEntry::try_new(
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

fn read_item(row: QueryResult) -> Result<ShopItem, DatabaseError> {
    let id: i64 = row.try_get("", "id")?;
    let guild: String = row.try_get("", "guild_id")?;
    let seller: String = row.try_get("", "seller_id")?;
    let kind: String = row.try_get("", "kind")?;
    let target: String = row.try_get("", "target")?;
    let revision: i64 = row.try_get("", "revision")?;
    let name: String = row.try_get("", "name")?;
    let description: String = row.try_get("", "description")?;
    let price: i64 = row.try_get("", "price")?;
    let deleted: Option<String> = row.try_get("", "deleted_at")?;
    let target = match kind.as_str() {
        "ROLE" => ItemTarget::Role(target.parse()?),
        "CONTENT" => {
            let key: String = row.try_get("", "file_key")?;
            let filename: String = row.try_get("", "filename")?;
            let size: i64 = row.try_get("", "size_bytes")?;
            let size = u64::try_from(size).map_err(|_| DatabaseError::InvalidData)?;
            ItemTarget::Content(ContentFile::try_new(key, filename, size)?)
        }
        _ => return Err(DatabaseError::InvalidData),
    };
    let details = ItemDetails::try_new(name, description, Price::try_from(price)?)?;
    ShopItem::try_new(
        ItemId::try_from(id)?,
        guild.parse()?,
        seller.parse()?,
        target,
        details,
        revision,
        deleted.is_some(),
    )
    .map_err(Into::into)
}

fn read_entry(row: QueryResult) -> Result<LedgerEntry, DatabaseError> {
    let id: i64 = row.try_get("", "id")?;
    let guild: String = row.try_get("", "guild_id")?;
    let user: String = row.try_get("", "user_id")?;
    let amount: i64 = row.try_get("", "amount")?;
    let code: String = row.try_get("", "reason")?;
    let key: String = row.try_get("", "event_key")?;
    let item: Option<i64> = row.try_get("", "item_id")?;
    let sale: Option<i64> = row.try_get("", "sale_of")?;
    let refund: Option<i64> = row.try_get("", "refund_of")?;
    let reason = match (code.as_str(), item, sale, refund) {
        ("MESSAGE", None, None, None) => LedgerReason::Message,
        ("REACTION", None, None, None) => LedgerReason::Reaction,
        ("REACTION_RECEIVED", None, None, None) => LedgerReason::ReactionReceived,
        ("REPLY_RECEIVED", None, None, None) => LedgerReason::ReplyReceived,
        ("TIP_SENT", None, None, None) => LedgerReason::TipSent,
        ("TIP_RECEIVED", None, None, None) => LedgerReason::TipReceived,
        ("TIMEOUT", None, None, None) => LedgerReason::Timeout,
        ("ADJUSTMENT", None, None, None) => LedgerReason::Adjustment,
        ("PURCHASE", Some(id), None, None) => LedgerReason::Purchase(ItemId::try_from(id)?),
        ("SALE", None, Some(id), None) => LedgerReason::Sale(LedgerEntryId::try_from(id)?),
        ("REFUND", None, None, Some(id)) => LedgerReason::Refund(LedgerEntryId::try_from(id)?),
        ("SALE_REFUND", None, None, Some(id)) => {
            LedgerReason::SaleRefund(LedgerEntryId::try_from(id)?)
        }
        _ => return Err(DatabaseError::InvalidData),
    };
    LedgerEntry::try_new(
        LedgerEntryId::try_from(id)?,
        guild.parse()?,
        user.parse()?,
        NotAmount::new(amount),
        reason,
        key,
    )
    .map_err(Into::into)
}

fn map_commit_error(error: DbErr) -> DatabaseError {
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