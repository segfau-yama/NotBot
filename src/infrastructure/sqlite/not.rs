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
