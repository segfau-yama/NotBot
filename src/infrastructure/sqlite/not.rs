use crate::domain;
use crate::infrastructure::types as values;
use sea_orm::QueryResult;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::{Actor, Balance, LedgerEntry, LedgerReason, NotAmount, UserBalance, UserId};

use super::{DatabaseError, SqliteDatabaseAccess};
use crate::infrastructure::RepositoryError;

pub struct NotRepository {
    database: SqliteDatabaseAccess,
}

impl NotRepository {
    pub fn new(database: SqliteDatabaseAccess) -> Self {
        Self { database }
    }

    pub async fn get_balance(&self, actor: Actor) -> Result<Balance, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let balance = session
            .get_balance(actor.guild_id(), actor.user_id())
            .await?;
        session.commit().await?;
        Ok(balance)
    }

    pub async fn get_ranking(
        &self,
        actor: Actor,
        limit: u32,
    ) -> Result<Vec<UserBalance>, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let ranking = session.get_ranking(actor.guild_id(), limit).await?;
        session.commit().await?;
        Ok(ranking)
    }

    /// 同じ依頼IDの再試行は、現在残高によらず元の増減を返す。
    pub async fn adjust_amount(
        &self,
        actor: Actor,
        user_id: UserId,
        amount: NotAmount,
        request_id: &str,
    ) -> Result<LedgerEntry, RepositoryError> {
        if !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied);
        }
        domain::validate_event_key(request_id)?;
        let event_key = format!("adjust-amount:{request_id}");
        let mut session = self.database.begin_write().await?;
        if let Some(existing) = session
            .find_by_event_key(actor.guild_id(), &event_key)
            .await?
        {
            if existing.user_id() != user_id
                || existing.reason() != LedgerReason::Adjustment
                || existing.amount() != amount
            {
                return Err(RepositoryError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let balance = session.get_balance(actor.guild_id(), user_id).await?;
        domain::Balance::apply(balance, amount)?;
        let entry = session
            .append_entry(domain::NewLedgerEntry::new(
                actor.guild_id(),
                user_id,
                amount,
                LedgerReason::Adjustment,
                event_key,
            )?)
            .await?;
        session.commit().await?;
        Ok(entry)
    }

    /// Discord側が確認した完了1分へ付与。獲得時刻を再試行でも変えない。
    pub async fn grant_voice_reward(
        &self,
        actor: Actor,
        earned_at: SystemTime,
    ) -> Result<Option<LedgerEntry>, RepositoryError> {
        let minute = earned_at
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs() / 60).ok())
            .filter(|minute| minute.checked_add(540).is_some())
            .ok_or(RepositoryError::InvalidRewardTime)?;
        let day = (minute + 540) / 1440;
        let prefix = format!("voice:{day}:{}:", actor.user_id());
        let key = format!("{prefix}{minute}");
        let mut session = self.database.begin_write().await?;
        if let Some(entry) = session.find_by_event_key(actor.guild_id(), &key).await? {
            if entry.user_id() != actor.user_id()
                || entry.reason() != LedgerReason::Adjustment
                || entry.amount().get() != 1
            {
                return Err(RepositoryError::RequestConflict);
            }
            session.commit().await?;
            return Ok(Some(entry));
        }
        if session.count_voice_rewards(actor, &prefix).await? >= 300 {
            session.commit().await?;
            return Ok(None);
        }
        let amount = domain::NotAmount::new(1);
        domain::Balance::apply(
            session
                .get_balance(actor.guild_id(), actor.user_id())
                .await?,
            amount,
        )?;
        // shortcut: 通話はADJUSTMENTとvoice:キーで記録する。専用の理由表示が必要ならDB制約も移行する。
        let entry = session
            .append_entry(domain::NewLedgerEntry::new(
                actor.guild_id(),
                actor.user_id(),
                amount,
                LedgerReason::Adjustment,
                key,
            )?)
            .await?;
        session.commit().await?;
        Ok(Some(entry))
    }

    /// Noneは制限による見送り。同じイベントは後日も元の付与／見送り結果を返す。
    pub async fn grant_reward(
        &self,
        actor: Actor,
        reason: LedgerReason,
        request_id: &str,
        now: SystemTime,
    ) -> Result<Option<LedgerEntry>, RepositoryError> {
        let amount = domain::NotAmount::new(match reason {
            LedgerReason::Message => 5,
            LedgerReason::Reaction => 1,
            LedgerReason::ReactionReceived => 10,
            LedgerReason::ReplyReceived => 20,
            _ => return Err(RepositoryError::InvalidRewardReason),
        });
        domain::validate_event_key(request_id)?;
        let key = format!("reward:{}:{request_id}", values::reason_code(reason));
        let mut session = self.database.begin_write().await?;
        if let Some((user, saved_reason, entry)) = session.find_reward_event(actor, &key).await? {
            if user != actor.user_id() || saved_reason != values::reason_code(reason) {
                return Err(RepositoryError::RequestConflict);
            }
            let entry = match entry {
                Some(id) => {
                    let entry = session
                        .find_entry(actor.guild_id(), id)
                        .await?
                        .ok_or(super::DatabaseError::InvalidData)?;
                    // 旧仕様の一律1 NOTも、当時の付与結果として復元する。
                    if entry.user_id() != user
                        || entry.reason() != reason
                        || (entry.amount() != amount && entry.amount().get() != 1)
                        || entry.event_key() != key
                    {
                        return Err(super::DatabaseError::InvalidData.into());
                    }
                    Some(entry)
                }
                None => None,
            };
            session.commit().await?;
            return Ok(entry);
        }
        let now = now
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .filter(|now| now.checked_add(32_400_000 + 86_400_000).is_some())
            .ok_or(RepositoryError::InvalidRewardTime)?;
        let entry = if session.reward_allowed(actor, reason, now).await? {
            domain::Balance::apply(
                session
                    .get_balance(actor.guild_id(), actor.user_id())
                    .await?,
                amount,
            )?;
            Some(
                session
                    .append_entry(domain::NewLedgerEntry::new(
                        actor.guild_id(),
                        actor.user_id(),
                        amount,
                        reason,
                        &key,
                    )?)
                    .await?,
            )
        } else {
            None
        };
        session
            .append_reward_event(
                actor,
                reason,
                &key,
                now,
                entry.as_ref().map(LedgerEntry::id),
            )
            .await?;
        session.commit().await?;
        Ok(entry)
    }

    /// 送信・受信の順。同じ依頼IDでは両行を照合し、現在残高に関係なく元の結果を返す。
    pub async fn transfer(
        &self,
        actor: Actor,
        recipient: UserId,
        amount: i64,
        request_id: &str,
    ) -> Result<(LedgerEntry, LedgerEntry), RepositoryError> {
        if actor.user_id() == recipient {
            return Err(RepositoryError::SelfTransfer);
        }
        if !matches!(amount, 100 | 500 | 1000) {
            return Err(RepositoryError::InvalidTransferAmount);
        }
        domain::validate_event_key(request_id)?;
        let sent_key = format!("transfer:{request_id}:sent");
        let received_key = format!("transfer:{request_id}:received");
        let mut session = self.database.begin_write().await?;
        let sent = session
            .find_by_event_key(actor.guild_id(), &sent_key)
            .await?;
        let received = session
            .find_by_event_key(actor.guild_id(), &received_key)
            .await?;
        match (sent, received) {
            (Some(sent), Some(received)) => {
                if sent.user_id() != actor.user_id()
                    || received.user_id() != recipient
                    || sent.reason() != LedgerReason::TipSent
                    || received.reason() != LedgerReason::TipReceived
                    || sent.amount().get() != -amount
                    || received.amount().get() != amount
                {
                    return Err(RepositoryError::RequestConflict);
                }
                session.commit().await?;
                return Ok((sent, received));
            }
            (None, None) => {}
            _ => return Err(super::DatabaseError::InvalidData.into()),
        }
        let sender_balance = session
            .get_balance(actor.guild_id(), actor.user_id())
            .await?;
        if sender_balance.get() < amount {
            return Err(RepositoryError::InsufficientBalance);
        }
        domain::Balance::apply(
            session.get_balance(actor.guild_id(), recipient).await?,
            domain::NotAmount::new(amount),
        )?;
        let sent = session
            .append_entry(domain::NewLedgerEntry::new(
                actor.guild_id(),
                actor.user_id(),
                domain::NotAmount::new(-amount),
                LedgerReason::TipSent,
                sent_key,
            )?)
            .await?;
        let received = session
            .append_entry(domain::NewLedgerEntry::new(
                actor.guild_id(),
                recipient,
                domain::NotAmount::new(amount),
                LedgerReason::TipReceived,
                received_key,
            )?)
            .await?;
        session.commit().await?;
        Ok((sent, received))
    }

    /// request_idは再試行でも変えない。同一内容なら確定済みの元取引を返す。
    pub async fn adjust_balance(
        &self,
        actor: Actor,
        user_id: UserId,
        target_balance: Balance,
        request_id: &str,
    ) -> Result<LedgerEntry, RepositoryError> {
        if !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied);
        }
        domain::validate_event_key(request_id)?;
        let event_key = format!("adjust:{request_id}");
        let mut session = self.database.begin_write().await?;
        if let Some(existing) = session
            .find_by_event_key(actor.guild_id(), &event_key)
            .await?
        {
            if existing.guild_id() != actor.guild_id()
                || existing.user_id() != user_id
                || existing.reason() != LedgerReason::Adjustment
            {
                return Err(RepositoryError::RequestConflict);
            }
            let original_target = session
                .get_balance_at_entry(actor.guild_id(), user_id, existing.id())
                .await?;
            if original_target != target_balance {
                return Err(RepositoryError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let current = session.get_balance(actor.guild_id(), user_id).await?;
        let amount = domain::NotAmount::checked_sub(
            domain::NotAmount::new(target_balance.get()),
            domain::NotAmount::new(current.get()),
        )?;
        let entry = domain::NewLedgerEntry::new(
            actor.guild_id(),
            user_id,
            amount,
            LedgerReason::Adjustment,
            event_key,
        )?;
        let entry = session.append_entry(entry).await?;
        session.commit().await?;
        Ok(entry)
    }
}

impl NotRepository {
    pub(super) fn read_entry(row: QueryResult) -> Result<LedgerEntry, DatabaseError> {
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
            ("PURCHASE", Some(id), None, None) => LedgerReason::Purchase(domain::ItemId::new(id)?),
            ("SALE", None, Some(id), None) => LedgerReason::Sale(domain::LedgerEntryId::new(id)?),
            ("REFUND", None, None, Some(id)) => {
                LedgerReason::Refund(domain::LedgerEntryId::new(id)?)
            }
            ("SALE_REFUND", None, None, Some(id)) => {
                LedgerReason::SaleRefund(domain::LedgerEntryId::new(id)?)
            }
            _ => return Err(DatabaseError::InvalidData),
        };
        domain::LedgerEntry::new(
            domain::LedgerEntryId::new(id)?,
            values::parse_guild_id(&guild)?,
            values::parse_user_id(&user)?,
            domain::NotAmount::new(amount),
            reason,
            key,
        )
        .map_err(Into::into)
    }
}
