use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, raw_sql};

use super::{DatabaseError, SqliteSession};
use crate::domain::{
    Balance, GuildId, ItemId, LedgerEntry, LedgerEntryId, LedgerReason, NewLedgerEntry, NotAmount,
    UserBalance, UserId,
};

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
                .map(read_entry)
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
            )).await?.into_iter().map(read_entry).collect()
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
