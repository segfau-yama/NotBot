use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::{
    Actor, Balance, LedgerEntry, LedgerReason, NewLedgerEntry, NotAmount, UserBalance, UserId,
    validate_event_key,
};

use super::SqliteDatabaseAccess;
use crate::infrastructure::UseCaseError;

pub struct NotUseCase {
    database: SqliteDatabaseAccess,
}

impl NotUseCase {
    pub fn new(database: SqliteDatabaseAccess) -> Self {
        Self { database }
    }

    pub async fn get_balance(&self, actor: Actor) -> Result<Balance, UseCaseError> {
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
    ) -> Result<Vec<UserBalance>, UseCaseError> {
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
    ) -> Result<LedgerEntry, UseCaseError> {
        if !actor.can_manage_not() {
            return Err(UseCaseError::PermissionDenied);
        }
        validate_event_key(request_id)?;
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
                return Err(UseCaseError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let balance = session.get_balance(actor.guild_id(), user_id).await?;
        balance.apply(amount)?;
        let entry = session
            .append_entry(NewLedgerEntry::try_new(
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

    /// Noneは制限による見送り。同じイベントは後日も元の付与／見送り結果を返す。
    pub async fn grant_reward(
        &self,
        actor: Actor,
        reason: LedgerReason,
        request_id: &str,
        now: SystemTime,
    ) -> Result<Option<LedgerEntry>, UseCaseError> {
        let amount = NotAmount::new(match reason {
            LedgerReason::Message => 5,
            LedgerReason::Reaction => 1,
            LedgerReason::ReactionReceived => 10,
            LedgerReason::ReplyReceived => 20,
            _ => return Err(UseCaseError::InvalidRewardReason),
        });
        validate_event_key(request_id)?;
        let key = format!("reward:{}:{request_id}", reason.code());
        let mut session = self.database.begin_write().await?;
        if let Some((user, saved_reason, entry)) = session.find_reward_event(actor, &key).await? {
            if user != actor.user_id() || saved_reason != reason.code() {
                return Err(UseCaseError::RequestConflict);
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
            .ok_or(UseCaseError::InvalidRewardTime)?;
        let entry = if session.reward_allowed(actor, reason, now).await? {
            session
                .get_balance(actor.guild_id(), actor.user_id())
                .await?
                .apply(amount)?;
            Some(
                session
                    .append_entry(NewLedgerEntry::try_new(
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
    ) -> Result<(LedgerEntry, LedgerEntry), UseCaseError> {
        if actor.user_id() == recipient {
            return Err(UseCaseError::SelfTransfer);
        }
        if !matches!(amount, 100 | 500 | 1000) {
            return Err(UseCaseError::InvalidTransferAmount);
        }
        validate_event_key(request_id)?;
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
                    return Err(UseCaseError::RequestConflict);
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
            return Err(UseCaseError::InsufficientBalance);
        }
        session
            .get_balance(actor.guild_id(), recipient)
            .await?
            .apply(NotAmount::new(amount))?;
        let sent = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                actor.user_id(),
                NotAmount::new(-amount),
                LedgerReason::TipSent,
                sent_key,
            )?)
            .await?;
        let received = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                recipient,
                NotAmount::new(amount),
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
    ) -> Result<LedgerEntry, UseCaseError> {
        if !actor.can_manage_not() {
            return Err(UseCaseError::PermissionDenied);
        }
        validate_event_key(request_id)?;
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
                return Err(UseCaseError::RequestConflict);
            }
            let original_target = session
                .get_balance_at_entry(actor.guild_id(), user_id, existing.id())
                .await?;
            if original_target != target_balance {
                return Err(UseCaseError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let current = session.get_balance(actor.guild_id(), user_id).await?;
        let amount =
            NotAmount::new(target_balance.get()).checked_sub(NotAmount::new(current.get()))?;
        let entry = NewLedgerEntry::try_new(
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
