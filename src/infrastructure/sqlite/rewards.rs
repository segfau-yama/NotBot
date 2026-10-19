use sea_orm::{ConnectionTrait, raw_sql};

use super::{DatabaseError, SqliteSession};
use crate::domain::{Actor, LedgerEntryId, LedgerReason, UserId};

impl SqliteSession {
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
                        user.parse()?,
                        row.try_get("", "reason")?,
                        entry.map(LedgerEntryId::try_from).transpose()?,
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
        let reason = reason.code();
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
        let reason = reason.code();
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
