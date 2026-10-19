use std::time::{Duration, SystemTime};

use crate::domain::{
    Actor, ContentFile, ItemDetails, ItemId, ItemTarget, LedgerEntry, LedgerEntryId, LedgerReason,
    NewLedgerEntry, NewShopItem, NotAmount, PurchaseQuote, PurchaseResult, ShopItem, TradeResult,
    ValidationError, validate_event_key,
};

use super::{DatabaseError, SqliteDatabaseAccess, SqliteSession};
use crate::infrastructure::{MAX_CONTENT_BYTES, StoredContent, UseCaseError};

pub struct ShopUseCase {
    database: SqliteDatabaseAccess,
}

impl ShopUseCase {
    pub fn new(database: SqliteDatabaseAccess) -> Self {
        Self { database }
    }

    /// nowは外部入力でなく、呼び出し側が受付時に採取したサーバー時刻を渡す。
    pub async fn prepare_purchase(
        &self,
        actor: Actor,
        item_id: ItemId,
        request_id: &str,
        now: SystemTime,
    ) -> Result<PurchaseQuote, UseCaseError> {
        validate_event_key(request_id)?;
        let expires_at = now
            .checked_add(Duration::from_secs(300))
            .ok_or(ValidationError::InvalidExpiration)?;
        let mut session = self.database.begin_read().await?;
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .ok_or(UseCaseError::ItemNotFound)?;
        validate_purchase(&mut session, actor, &item).await?;
        session.commit().await?;
        Ok(PurchaseQuote::new(
            request_id.to_owned(),
            actor.user_id(),
            item,
            expires_at,
        ))
    }

    /// 受付時のサーバー時刻で期限を判定する。確定済みの同一確認は期限を再判定しない。
    pub async fn purchase(
        &self,
        actor: Actor,
        quote: &PurchaseQuote,
        now: SystemTime,
    ) -> Result<PurchaseResult, UseCaseError> {
        if actor.user_id() != quote.buyer_id() || actor.guild_id() != quote.item().guild_id() {
            return Err(UseCaseError::PermissionDenied);
        }
        let buyer_key = format!("content:{}:buyer", quote.id());
        let seller_key = format!("content:{}:seller", quote.id());
        let mut session = self.database.begin_write().await?;
        let buyer = session
            .find_by_event_key(actor.guild_id(), &buyer_key)
            .await?;
        let seller = session
            .find_by_event_key(actor.guild_id(), &seller_key)
            .await?;
        let amount = NotAmount::new(quote.item().details().price().get());
        match (buyer, seller) {
            (Some(buyer), Some(seller)) => {
                if buyer.guild_id() != actor.guild_id()
                    || buyer.user_id() != actor.user_id()
                    || buyer.reason() != LedgerReason::Purchase(quote.item().id())
                    || buyer.amount() != amount.negated()?
                    || seller.guild_id() != actor.guild_id()
                    || seller.user_id() != quote.item().seller_id()
                    || seller.reason() != LedgerReason::Sale(buyer.id())
                    || seller.amount() != amount
                {
                    return Err(UseCaseError::RequestConflict);
                }
                let result = purchase_result(&mut session, buyer, seller).await?;
                if result.item().target() != quote.item().target() {
                    return Err(DatabaseError::InvalidData.into());
                }
                session.commit().await?;
                return Ok(result);
            }
            (None, None) => {}
            _ => return Err(DatabaseError::InvalidData.into()),
        }
        if now >= quote.expires_at() {
            return Err(UseCaseError::QuoteExpired);
        }
        let item = session
            .find_item(actor.guild_id(), quote.item().id())
            .await?
            .ok_or(UseCaseError::ItemNotFound)?;
        if item.revision() != quote.item().revision()
            || item.details().price() != quote.item().details().price()
        {
            return Err(UseCaseError::RevisionConflict);
        }
        validate_purchase(&mut session, actor, &item).await?;
        let buyer = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                actor.user_id(),
                amount.negated()?,
                LedgerReason::Purchase(item.id()),
                buyer_key,
            )?)
            .await?;
        let seller = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                item.seller_id(),
                amount,
                LedgerReason::Sale(buyer.id()),
                seller_key,
            )?)
            .await?;
        session.commit().await?;
        Ok(PurchaseResult::new(item, buyer, seller))
    }

    pub async fn find_purchase_by_confirmation(
        &self,
        actor: Actor,
        confirmation_id: &str,
    ) -> Result<Option<PurchaseResult>, UseCaseError> {
        validate_event_key(confirmation_id)?;
        let mut session = self.database.begin_read().await?;
        let buyer = session
            .find_by_event_key(
                actor.guild_id(),
                &format!("content:{confirmation_id}:buyer"),
            )
            .await?;
        let seller = session
            .find_by_event_key(
                actor.guild_id(),
                &format!("content:{confirmation_id}:seller"),
            )
            .await?;
        let result = match (buyer, seller) {
            (None, None) => None,
            (Some(buyer), Some(seller)) => {
                if buyer.user_id() != actor.user_id() {
                    return Err(UseCaseError::PermissionDenied);
                }
                Some(purchase_result(&mut session, buyer, seller).await?)
            }
            _ => return Err(DatabaseError::InvalidData.into()),
        };
        session.commit().await?;
        Ok(result)
    }

    /// 本人の未返金購入を新しい購入ID順に返す。削除済みの商品も含む。
    pub async fn list_purchases(&self, actor: Actor) -> Result<Vec<PurchaseResult>, UseCaseError> {
        let mut session = self.database.begin_read().await?;
        let entries = session
            .list_purchase_entries(actor.guild_id(), actor.user_id())
            .await?;
        let mut results = Vec::new();
        // ponytail: 購入ごとの照会で始める。件数による遅延を計測したらJOINとページングへ集約する。
        for buyer in entries {
            let purchase = load_purchase(&mut session, buyer).await?;
            if refund_state(&mut session, &purchase).await?.is_none() {
                results.push(purchase);
            }
        }
        session.commit().await?;
        Ok(results)
    }

    /// 読み取り時点の購入権限を確認し、配送に必要な固定のファイル情報だけを返す。
    pub async fn get_content_for_purchase(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
    ) -> Result<ContentFile, UseCaseError> {
        let mut session = self.database.begin_read().await?;
        let buyer = session
            .find_entry(actor.guild_id(), purchase_id)
            .await?
            .ok_or(UseCaseError::PurchaseNotFound)?;
        if buyer.user_id() != actor.user_id() {
            return Err(UseCaseError::PermissionDenied);
        }
        let purchase = load_purchase(&mut session, buyer).await?;
        if refund_state(&mut session, &purchase).await?.is_some() {
            return Err(UseCaseError::AlreadyRefunded);
        }
        let ItemTarget::Content(file) = purchase.item().target() else {
            return Err(UseCaseError::ItemNotFound);
        };
        let file = file.clone();
        session.commit().await?;
        Ok(file)
    }

    /// NOT管理者の明示操作だけを受け付ける。同じ依頼IDの再試行では元の2行を返す。
    pub async fn refund(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
        request_id: &str,
    ) -> Result<TradeResult, UseCaseError> {
        if !actor.can_manage_not() {
            return Err(UseCaseError::PermissionDenied);
        }
        validate_event_key(request_id)?;
        let mut session = self.database.begin_write().await?;
        let buyer = session
            .find_entry(actor.guild_id(), purchase_id)
            .await?
            .ok_or(UseCaseError::PurchaseNotFound)?;
        let purchase = load_purchase(&mut session, buyer).await?;
        let buyer_key = format!("refund:{request_id}:buyer");
        let seller_key = format!("refund:{request_id}:seller");
        let buyer = session
            .find_by_event_key(actor.guild_id(), &buyer_key)
            .await?;
        let seller = session
            .find_by_event_key(actor.guild_id(), &seller_key)
            .await?;
        match (buyer, seller) {
            (Some(buyer), Some(seller)) => {
                let result = checked_refund(&purchase, buyer, seller)
                    .map_err(|_| UseCaseError::RequestConflict)?;
                session.commit().await?;
                return Ok(result);
            }
            (None, None) => {}
            _ => return Err(DatabaseError::InvalidData.into()),
        }
        if refund_state(&mut session, &purchase).await?.is_some() {
            return Err(UseCaseError::AlreadyRefunded);
        }
        let buyer_amount = purchase.buyer().amount().negated()?;
        let seller_amount = purchase.seller().amount().negated()?;
        let seller_balance = session
            .get_balance(actor.guild_id(), purchase.seller().user_id())
            .await?;
        if seller_balance.get() < buyer_amount.get() {
            return Err(UseCaseError::InsufficientBalance);
        }
        seller_balance.apply(seller_amount)?;
        session
            .get_balance(actor.guild_id(), purchase.buyer().user_id())
            .await?
            .apply(buyer_amount)?;
        let buyer = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                purchase.buyer().user_id(),
                buyer_amount,
                LedgerReason::Refund(purchase_id),
                buyer_key,
            )?)
            .await?;
        let seller = session
            .append_entry(NewLedgerEntry::try_new(
                actor.guild_id(),
                purchase.seller().user_id(),
                seller_amount,
                LedgerReason::SaleRefund(purchase.seller().id()),
                seller_key,
            )?)
            .await?;
        session.commit().await?;
        Ok(TradeResult::new(buyer, seller))
    }

    pub async fn create(
        &self,
        actor: Actor,
        details: ItemDetails,
        content: StoredContent,
    ) -> Result<ShopItem, UseCaseError> {
        if actor.guild_id() != content.actor.guild_id()
            || actor.user_id() != content.actor.user_id()
        {
            return Err(UseCaseError::PermissionDenied);
        }
        if content.file.size_bytes() > MAX_CONTENT_BYTES {
            return Err(ValidationError::InvalidFileSize.into());
        }
        let mut session = self.database.begin_write().await?;
        if let Some(existing) = session
            .find_content_item(actor.guild_id(), content.file.key())
            .await?
        {
            let original = session
                .find_item_revision(actor.guild_id(), existing.id(), 1)
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            if original.seller_id() != actor.user_id()
                || original.target() != &ItemTarget::Content(content.file)
                || original.details() != &details
                || original.is_deleted()
            {
                return Err(UseCaseError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let item = NewShopItem::new(details, ItemTarget::Content(content.file));
        let item = session.append_content_item(actor, item).await?;
        session.commit().await?;
        Ok(item)
    }

    /// 一般閲覧。削除済みを含む内部照会にはSqliteSession::find_itemを使う。
    pub async fn find(&self, actor: Actor, item_id: ItemId) -> Result<ShopItem, UseCaseError> {
        let mut session = self.database.begin_read().await?;
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .filter(|item| !item.is_deleted())
            .ok_or(UseCaseError::ItemNotFound)?;
        session.commit().await?;
        Ok(item)
    }

    pub async fn list(&self, actor: Actor) -> Result<Vec<ShopItem>, UseCaseError> {
        let mut session = self.database.begin_read().await?;
        let items = session.list_active_items(actor.guild_id()).await?;
        session.commit().await?;
        Ok(items)
    }

    pub async fn update(
        &self,
        actor: Actor,
        item_id: ItemId,
        expected_revision: i64,
        details: ItemDetails,
    ) -> Result<ShopItem, UseCaseError> {
        let next_revision = next_revision(expected_revision)?;
        let mut session = self.database.begin_write().await?;
        let current = owned_item(&mut session, actor, item_id).await?;
        if current.revision() != expected_revision {
            if let Some(applied) = session
                .find_item_revision(actor.guild_id(), item_id, next_revision)
                .await?
                .filter(|item| !item.is_deleted() && item.details() == &details)
            {
                session.commit().await?;
                return Ok(applied);
            }
            return Err(UseCaseError::RevisionConflict);
        }
        if current.is_deleted() {
            return Err(UseCaseError::ItemDeleted);
        }
        let next = ShopItem::try_new(
            current.id(),
            current.guild_id(),
            current.seller_id(),
            current.target().clone(),
            details,
            next_revision,
            false,
        )?;
        let next = session.append_item_revision(next).await?;
        session.commit().await?;
        Ok(next)
    }

    pub async fn delete(
        &self,
        actor: Actor,
        item_id: ItemId,
        expected_revision: i64,
    ) -> Result<ShopItem, UseCaseError> {
        let next_revision = next_revision(expected_revision)?;
        let mut session = self.database.begin_write().await?;
        let current = owned_item(&mut session, actor, item_id).await?;
        if current.is_deleted() {
            session.commit().await?;
            return Ok(current);
        }
        if current.revision() != expected_revision {
            return Err(UseCaseError::RevisionConflict);
        }
        let next = ShopItem::try_new(
            current.id(),
            current.guild_id(),
            current.seller_id(),
            current.target().clone(),
            current.details().clone(),
            next_revision,
            true,
        )?;
        let next = session.append_item_revision(next).await?;
        session.commit().await?;
        Ok(next)
    }
}

async fn load_purchase(
    session: &mut SqliteSession,
    buyer: LedgerEntry,
) -> Result<PurchaseResult, UseCaseError> {
    if !matches!(buyer.reason(), LedgerReason::Purchase(_)) {
        return Err(UseCaseError::PurchaseNotFound);
    }
    let seller = session
        .find_sale(buyer.guild_id(), buyer.id())
        .await?
        .ok_or(DatabaseError::InvalidData)?;
    purchase_result(session, buyer, seller).await
}

async fn purchase_result(
    session: &mut SqliteSession,
    buyer: LedgerEntry,
    seller: LedgerEntry,
) -> Result<PurchaseResult, UseCaseError> {
    let LedgerReason::Purchase(item_id) = buyer.reason() else {
        return Err(DatabaseError::InvalidData.into());
    };
    let item = session
        .find_item(buyer.guild_id(), item_id)
        .await?
        .ok_or(DatabaseError::InvalidData)?;
    if seller.guild_id() != buyer.guild_id()
        || seller.user_id() != item.seller_id()
        || buyer.user_id() == seller.user_id()
        || seller.reason() != LedgerReason::Sale(buyer.id())
        || seller.amount()
            != buyer
                .amount()
                .negated()
                .map_err(|_| DatabaseError::InvalidData)?
    {
        return Err(DatabaseError::InvalidData.into());
    }
    Ok(PurchaseResult::new(item, buyer, seller))
}

async fn refund_state(
    session: &mut SqliteSession,
    purchase: &PurchaseResult,
) -> Result<Option<TradeResult>, UseCaseError> {
    let buyer = session
        .find_refund(purchase.buyer().guild_id(), purchase.buyer().id())
        .await?;
    let seller = session
        .find_refund(purchase.seller().guild_id(), purchase.seller().id())
        .await?;
    match (buyer, seller) {
        (None, None) => Ok(None),
        (Some(buyer), Some(seller)) => Ok(Some(checked_refund(purchase, buyer, seller)?)),
        _ => Err(DatabaseError::InvalidData.into()),
    }
}

fn checked_refund(
    purchase: &PurchaseResult,
    buyer: LedgerEntry,
    seller: LedgerEntry,
) -> Result<TradeResult, DatabaseError> {
    if buyer.guild_id() != purchase.buyer().guild_id()
        || seller.guild_id() != purchase.seller().guild_id()
        || buyer.user_id() != purchase.buyer().user_id()
        || seller.user_id() != purchase.seller().user_id()
        || buyer.reason() != LedgerReason::Refund(purchase.buyer().id())
        || seller.reason() != LedgerReason::SaleRefund(purchase.seller().id())
        || buyer.amount()
            != purchase
                .buyer()
                .amount()
                .negated()
                .map_err(|_| DatabaseError::InvalidData)?
        || seller.amount()
            != purchase
                .seller()
                .amount()
                .negated()
                .map_err(|_| DatabaseError::InvalidData)?
    {
        return Err(DatabaseError::InvalidData);
    }
    Ok(TradeResult::new(buyer, seller))
}

async fn validate_purchase(
    session: &mut SqliteSession,
    actor: Actor,
    item: &ShopItem,
) -> Result<(), UseCaseError> {
    if item.is_deleted() {
        return Err(UseCaseError::ItemDeleted);
    }
    let ItemTarget::Content(file) = item.target() else {
        return Err(UseCaseError::ItemNotFound);
    };
    if file.size_bytes() > MAX_CONTENT_BYTES {
        return Err(ValidationError::InvalidFileSize.into());
    }
    if item.seller_id() == actor.user_id() {
        return Err(UseCaseError::SelfPurchase);
    }
    if session
        .has_purchase(actor.guild_id(), actor.user_id(), item.id())
        .await?
    {
        return Err(UseCaseError::AlreadyPurchased);
    }
    let amount = NotAmount::new(item.details().price().get());
    let buyer_balance = session
        .get_balance(actor.guild_id(), actor.user_id())
        .await?;
    if buyer_balance.get() < amount.get() {
        return Err(UseCaseError::InsufficientBalance);
    }
    buyer_balance.apply(amount.negated()?)?;
    session
        .get_balance(actor.guild_id(), item.seller_id())
        .await?
        .apply(amount)?;
    Ok(())
}

fn next_revision(revision: i64) -> Result<i64, UseCaseError> {
    if revision <= 0 {
        return Err(ValidationError::InvalidRevision.into());
    }
    revision
        .checked_add(1)
        .ok_or(ValidationError::InvalidRevision.into())
}

async fn owned_item(
    session: &mut SqliteSession,
    actor: Actor,
    item_id: ItemId,
) -> Result<ShopItem, UseCaseError> {
    let item = session
        .find_item(actor.guild_id(), item_id)
        .await?
        .ok_or(UseCaseError::ItemNotFound)?;
    if item.seller_id() != actor.user_id() {
        return Err(UseCaseError::PermissionDenied);
    }
    Ok(item)
}
