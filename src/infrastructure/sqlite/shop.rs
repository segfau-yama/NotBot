use crate::infrastructure::ValidationError;
use crate::infrastructure::types as values;
use sea_orm::QueryResult;
use std::time::{Duration, SystemTime};

use crate::domain::{
    Actor, ChannelId, ContentFile, GuildId, ItemDetails, ItemId, ItemTarget, LedgerEntry,
    LedgerEntryId, LedgerReason, MessageId, PurchaseQuote, PurchaseResult, RefundResult, RoleId,
    ShopItem,
};

use super::{DatabaseError, SqliteDatabaseAccess, SqliteSession};
use crate::domain::StoredContent;
use crate::infrastructure::{MAX_CONTENT_BYTES, RepositoryError};

pub struct ShopRepository {
    database: SqliteDatabaseAccess,
}

impl ShopRepository {
    pub fn new(database: SqliteDatabaseAccess) -> Self {
        Self { database }
    }

    /// 永続化前と履歴復元で共用する購入結果の整合性チェック。
    pub fn purchase_result(
        item: ShopItem,
        buyer: LedgerEntry,
        seller: Option<LedgerEntry>,
    ) -> Result<PurchaseResult, DatabaseError> {
        if buyer.guild_id() != item.guild_id()
            || buyer.reason() != LedgerReason::Purchase(item.id())
        {
            return Err(DatabaseError::InvalidData);
        }
        let amount = values::negated(buyer.amount()).map_err(|_| DatabaseError::InvalidData)?;
        if amount.get() <= 0
            || (matches!(item.target(), ItemTarget::Content(_))
                && (seller.is_none() || buyer.user_id() == item.seller_id()))
        {
            return Err(DatabaseError::InvalidData);
        }
        if let Some(sale) = &seller
            && (sale.guild_id() != buyer.guild_id()
                || sale.user_id() != item.seller_id()
                || sale.reason() != LedgerReason::Sale(buyer.id())
                || sale.amount() != amount)
        {
            return Err(DatabaseError::InvalidData);
        }
        Ok(PurchaseResult::new(item, buyer, seller))
    }

    /// nowは外部入力でなく、呼び出し側が受付時に採取したサーバー時刻を渡す。
    pub async fn prepare_purchase(
        &self,
        actor: Actor,
        item_id: ItemId,
        request_id: &str,
        now: SystemTime,
    ) -> Result<PurchaseQuote, RepositoryError> {
        values::validate_event_key(request_id)?;
        let expires_at = now
            .checked_add(Duration::from_secs(300))
            .ok_or(ValidationError::InvalidExpiration)?;
        let mut session = self.database.begin_read().await?;
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .ok_or(RepositoryError::ItemNotFound)?;
        ShopRepository::validate_purchase(&mut session, actor, &item).await?;
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
    ) -> Result<PurchaseResult, RepositoryError> {
        if actor.user_id() != quote.buyer_id() || actor.guild_id() != quote.item().guild_id() {
            return Err(RepositoryError::PermissionDenied);
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
        let amount = values::amount(quote.item().details().price().get());
        match (buyer, seller) {
            (Some(buyer), seller) => {
                if seller.is_none() && matches!(quote.item().target(), ItemTarget::Content(_)) {
                    return Err(DatabaseError::InvalidData.into());
                }
                if buyer.guild_id() != actor.guild_id()
                    || buyer.user_id() != actor.user_id()
                    || buyer.reason() != LedgerReason::Purchase(quote.item().id())
                    || buyer.amount() != values::negated(amount)?
                {
                    return Err(RepositoryError::RequestConflict);
                }
                let result =
                    ShopRepository::load_purchase_result(&mut session, buyer, seller).await?;
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
            return Err(RepositoryError::QuoteExpired);
        }
        let item = session
            .find_item(actor.guild_id(), quote.item().id())
            .await?
            .ok_or(RepositoryError::ItemNotFound)?;
        if item.revision() != quote.item().revision()
            || item.details().price() != quote.item().details().price()
        {
            return Err(RepositoryError::RevisionConflict);
        }
        ShopRepository::validate_purchase(&mut session, actor, &item).await?;
        let buyer = session
            .append_entry(values::new_ledger_entry(
                actor.guild_id(),
                actor.user_id(),
                values::negated(amount)?,
                LedgerReason::Purchase(item.id()),
                buyer_key,
            )?)
            .await?;
        let seller = if matches!(item.target(), ItemTarget::Content(_)) {
            Some(
                session
                    .append_entry(values::new_ledger_entry(
                        actor.guild_id(),
                        item.seller_id(),
                        amount,
                        LedgerReason::Sale(buyer.id()),
                        seller_key,
                    )?)
                    .await?,
            )
        } else {
            None
        };
        let result = Self::purchase_result(item, buyer, seller)?;
        session.commit().await?;
        Ok(result)
    }

    pub async fn find_purchase_by_confirmation(
        &self,
        actor: Actor,
        confirmation_id: &str,
    ) -> Result<Option<PurchaseResult>, RepositoryError> {
        values::validate_event_key(confirmation_id)?;
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
            (Some(buyer), seller) => {
                if buyer.user_id() != actor.user_id() {
                    return Err(RepositoryError::PermissionDenied);
                }
                Some(ShopRepository::load_purchase_result(&mut session, buyer, seller).await?)
            }
            _ => return Err(DatabaseError::InvalidData.into()),
        };
        session.commit().await?;
        Ok(result)
    }

    /// 本人の未返金購入を新しい購入ID順に返す。削除済みの商品も含む。
    pub async fn list_purchases(
        &self,
        actor: Actor,
    ) -> Result<Vec<PurchaseResult>, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let entries = session
            .list_purchase_entries(actor.guild_id(), actor.user_id())
            .await?;
        let mut results = Vec::new();
        // ponytail: 購入ごとの照会で始める。件数による遅延を計測したらJOINとページングへ集約する。
        for buyer in entries {
            if let Some(purchase) = ShopRepository::unrefunded_purchase(&mut session, buyer).await?
            {
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
    ) -> Result<ContentFile, RepositoryError> {
        let purchase = self.get_purchase(actor, purchase_id).await?;
        if purchase.buyer().user_id() != actor.user_id() {
            return Err(RepositoryError::PermissionDenied);
        }
        let ItemTarget::Content(file) = purchase.item().target() else {
            return Err(RepositoryError::ItemNotFound);
        };
        Ok(file.clone())
    }

    /// 本人または管理者の未返金購入。削除済み商品も参照する。
    pub async fn get_purchase(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
    ) -> Result<PurchaseResult, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let buyer = session
            .find_entry(actor.guild_id(), purchase_id)
            .await?
            .ok_or(RepositoryError::PurchaseNotFound)?;
        if buyer.user_id() != actor.user_id() && !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied);
        }
        let purchase = ShopRepository::unrefunded_purchase(&mut session, buyer)
            .await?
            .ok_or(RepositoryError::AlreadyRefunded)?;
        session.commit().await?;
        Ok(purchase)
    }

    pub(crate) async fn list_role_purchases(
        &self,
        guild: GuildId,
    ) -> Result<Vec<PurchaseResult>, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let entries = session.list_role_purchase_entries(guild).await?;
        let mut result = Vec::new();
        for entry in entries {
            if let Some(purchase) = ShopRepository::unrefunded_purchase(&mut session, entry).await?
            {
                result.push(purchase)
            }
        }
        session.commit().await?;
        Ok(result)
    }

    /// NOT管理者の明示操作だけを受け付ける。同じ依頼IDの再試行では元の履歴を返す。
    pub async fn refund(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
        request_id: &str,
    ) -> Result<RefundResult, RepositoryError> {
        if !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied);
        }
        self.refund_checked(actor, purchase_id, request_id, false)
            .await
    }

    /// Discord側が未付与を照合した後にだけ呼ぶ。購入者本人のロール購入に限定する。
    pub(crate) async fn refund_role_delivery(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
    ) -> Result<RefundResult, RepositoryError> {
        self.refund_checked(
            actor,
            purchase_id,
            &format!("role-delivery-{purchase_id}"),
            true,
        )
        .await
    }

    async fn refund_checked(
        &self,
        actor: Actor,
        purchase_id: LedgerEntryId,
        request_id: &str,
        role_delivery: bool,
    ) -> Result<RefundResult, RepositoryError> {
        values::validate_event_key(request_id)?;
        let mut session = self.database.begin_write().await?;
        let buyer = session
            .find_entry(actor.guild_id(), purchase_id)
            .await?
            .ok_or(RepositoryError::PurchaseNotFound)?;
        let purchase = ShopRepository::load_purchase(&mut session, buyer).await?;
        if role_delivery
            && (purchase.buyer().user_id() != actor.user_id()
                || !matches!(purchase.item().target(), ItemTarget::Role(_)))
        {
            return Err(RepositoryError::PermissionDenied);
        }
        let buyer_key = format!("refund:{request_id}:buyer");
        let seller_key = format!("refund:{request_id}:seller");
        let buyer = session
            .find_by_event_key(actor.guild_id(), &buyer_key)
            .await?;
        let seller = session
            .find_by_event_key(actor.guild_id(), &seller_key)
            .await?;
        match (buyer, seller) {
            (Some(buyer), seller) if seller.is_some() == purchase.seller().is_some() => {
                let result = ShopRepository::refund_result(&purchase, buyer, seller)
                    .map_err(|_| RepositoryError::RequestConflict)?;
                session.commit().await?;
                return Ok(result);
            }
            (None, None) => {}
            _ => return Err(DatabaseError::InvalidData.into()),
        }
        if ShopRepository::refund_state(&mut session, &purchase)
            .await?
            .is_some()
        {
            return Err(RepositoryError::AlreadyRefunded);
        }
        let buyer_amount = values::negated(purchase.buyer().amount())?;
        if purchase
            .seller()
            .is_none_or(|seller| seller.user_id() != purchase.buyer().user_id())
        {
            values::apply_balance(
                session
                    .get_balance(actor.guild_id(), purchase.buyer().user_id())
                    .await?,
                buyer_amount,
            )?;
        }
        // 同一ユーザーの相殺でもSUMが途中で整数上限を超えないよう、負数行を先に記録する。
        let seller = if let Some(seller) = purchase.seller() {
            let seller_amount = values::negated(seller.amount())?;
            if seller.user_id() != purchase.buyer().user_id() {
                let balance = session
                    .get_balance(actor.guild_id(), seller.user_id())
                    .await?;
                if balance.get() < buyer_amount.get() {
                    return Err(RepositoryError::InsufficientBalance);
                }
                values::apply_balance(balance, seller_amount)?;
            }
            Some(
                session
                    .append_entry(values::new_ledger_entry(
                        actor.guild_id(),
                        seller.user_id(),
                        seller_amount,
                        LedgerReason::SaleRefund(seller.id()),
                        seller_key,
                    )?)
                    .await?,
            )
        } else {
            None
        };
        let buyer = session
            .append_entry(values::new_ledger_entry(
                actor.guild_id(),
                purchase.buyer().user_id(),
                buyer_amount,
                LedgerReason::Refund(purchase_id),
                buyer_key,
            )?)
            .await?;
        let result = ShopRepository::refund_result(&purchase, buyer, seller)?;
        session.commit().await?;
        Ok(result)
    }

    pub async fn create(
        &self,
        actor: Actor,
        details: ItemDetails,
        content: StoredContent,
    ) -> Result<ShopItem, RepositoryError> {
        if actor.guild_id() != content.actor().guild_id()
            || actor.user_id() != content.actor().user_id()
        {
            return Err(RepositoryError::PermissionDenied);
        }
        if content.file().size_bytes() > MAX_CONTENT_BYTES {
            return Err(ValidationError::InvalidFileSize.into());
        }
        let mut session = self.database.begin_write().await?;
        if let Some(existing) = session
            .find_content_item(actor.guild_id(), content.file().key())
            .await?
        {
            let original = session
                .find_item_revision(actor.guild_id(), existing.id(), 1)
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            if original.seller_id() != actor.user_id()
                || original.target() != &ItemTarget::Content(content.into_file())
                || original.details() != &details
                || original.is_deleted()
            {
                return Err(RepositoryError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let item = values::new_item(details, ItemTarget::Content(content.into_file()));
        let item = session.append_content_item(actor, item).await?;
        session.commit().await?;
        Ok(item)
    }

    pub async fn create_role(
        &self,
        actor: Actor,
        role: RoleId,
        details: ItemDetails,
    ) -> Result<ShopItem, RepositoryError> {
        if !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied);
        }
        let mut session = self.database.begin_write().await?;
        if let Some(existing) = session.find_role_item(actor.guild_id(), role).await? {
            let original = session
                .find_item_revision(actor.guild_id(), existing.id(), 1)
                .await?
                .ok_or(DatabaseError::InvalidData)?;
            if original.seller_id() != actor.user_id() || original.details() != &details {
                return Err(RepositoryError::RequestConflict);
            }
            session.commit().await?;
            return Ok(existing);
        }
        let item = session.append_role_item(actor, role, details).await?;
        session.commit().await?;
        Ok(item)
    }

    /// 一般閲覧。削除済みを含む内部照会にはSqliteSession::find_itemを使う。
    pub async fn find(&self, actor: Actor, item_id: ItemId) -> Result<ShopItem, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .filter(|item| !item.is_deleted())
            .ok_or(RepositoryError::ItemNotFound)?;
        session.commit().await?;
        Ok(item)
    }

    pub async fn list(&self, actor: Actor) -> Result<Vec<ShopItem>, RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let items = session.list_active_items(actor.guild_id()).await?;
        session.commit().await?;
        Ok(items)
    }

    /// 販売終了も公開へ反映するため、削除版を含めて出品者本人に返す。
    pub async fn get_publication(
        &self,
        actor: Actor,
        item_id: ItemId,
    ) -> Result<(ShopItem, Option<(i64, ChannelId, MessageId)>), RepositoryError> {
        let mut session = self.database.begin_read().await?;
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .ok_or(RepositoryError::ItemNotFound)?;
        if item.seller_id() != actor.user_id() {
            return Err(RepositoryError::PermissionDenied);
        }
        let publication = session.find_publication(actor.guild_id(), item_id).await?;
        session.commit().await?;
        Ok((item, publication))
    }

    pub async fn record_publication(
        &self,
        actor: Actor,
        item: &ShopItem,
        channel: ChannelId,
        message: MessageId,
    ) -> Result<(), RepositoryError> {
        if item.guild_id() != actor.guild_id() || item.seller_id() != actor.user_id() {
            return Err(RepositoryError::PermissionDenied);
        }
        let mut session = self.database.begin_write().await?;
        session.append_publication(item, channel, message).await?;
        session.commit().await?;
        Ok(())
    }

    pub async fn update(
        &self,
        actor: Actor,
        item_id: ItemId,
        expected_revision: i64,
        details: ItemDetails,
    ) -> Result<ShopItem, RepositoryError> {
        let next_revision = ShopRepository::next_revision(expected_revision)?;
        let mut session = self.database.begin_write().await?;
        let current = ShopRepository::owned_item(&mut session, actor, item_id).await?;
        if current.revision() != expected_revision {
            if let Some(applied) = session
                .find_item_revision(actor.guild_id(), item_id, next_revision)
                .await?
                .filter(|item| !item.is_deleted() && item.details() == &details)
            {
                session.commit().await?;
                return Ok(applied);
            }
            return Err(RepositoryError::RevisionConflict);
        }
        if current.is_deleted() {
            return Err(RepositoryError::ItemDeleted);
        }
        let next = values::shop_item(
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
    ) -> Result<ShopItem, RepositoryError> {
        let next_revision = ShopRepository::next_revision(expected_revision)?;
        let mut session = self.database.begin_write().await?;
        let current = ShopRepository::owned_item(&mut session, actor, item_id).await?;
        if current.is_deleted() {
            session.commit().await?;
            return Ok(current);
        }
        if current.revision() != expected_revision {
            return Err(RepositoryError::RevisionConflict);
        }
        let next = values::shop_item(
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

impl ShopRepository {
    async fn unrefunded_purchase(
        session: &mut SqliteSession,
        buyer: LedgerEntry,
    ) -> Result<Option<PurchaseResult>, RepositoryError> {
        let purchase = ShopRepository::load_purchase(session, buyer).await?;
        let refunded = ShopRepository::refund_state(session, &purchase)
            .await?
            .is_some();
        Ok((!refunded).then_some(purchase))
    }
    async fn load_purchase(
        session: &mut SqliteSession,
        buyer: LedgerEntry,
    ) -> Result<PurchaseResult, RepositoryError> {
        if !matches!(buyer.reason(), LedgerReason::Purchase(_)) {
            return Err(RepositoryError::PurchaseNotFound);
        }
        let seller = session.find_sale(buyer.guild_id(), buyer.id()).await?;
        ShopRepository::load_purchase_result(session, buyer, seller).await
    }
    async fn load_purchase_result(
        session: &mut SqliteSession,
        buyer: LedgerEntry,
        seller: Option<LedgerEntry>,
    ) -> Result<PurchaseResult, RepositoryError> {
        let LedgerReason::Purchase(item_id) = buyer.reason() else {
            return Err(DatabaseError::InvalidData.into());
        };
        let item = session
            .find_item(buyer.guild_id(), item_id)
            .await?
            .ok_or(DatabaseError::InvalidData)?;
        Ok(ShopRepository::purchase_result(item, buyer, seller)?)
    }
    async fn refund_state(
        session: &mut SqliteSession,
        purchase: &PurchaseResult,
    ) -> Result<Option<RefundResult>, RepositoryError> {
        let buyer = session
            .find_refund(purchase.buyer().guild_id(), purchase.buyer().id())
            .await?;
        let seller = if let Some(seller) = purchase.seller() {
            session.find_refund(seller.guild_id(), seller.id()).await?
        } else {
            None
        };
        match (buyer, seller) {
            (None, None) => Ok(None),
            (Some(buyer), seller) => Ok(Some(ShopRepository::refund_result(
                purchase, buyer, seller,
            )?)),
            _ => Err(DatabaseError::InvalidData.into()),
        }
    }
    pub fn refund_result(
        purchase: &PurchaseResult,
        buyer: LedgerEntry,
        seller: Option<LedgerEntry>,
    ) -> Result<RefundResult, DatabaseError> {
        if buyer.guild_id() != purchase.buyer().guild_id()
            || buyer.user_id() != purchase.buyer().user_id()
            || buyer.reason() != LedgerReason::Refund(purchase.buyer().id())
            || buyer.amount()
                != values::negated(purchase.buyer().amount())
                    .map_err(|_| DatabaseError::InvalidData)?
        {
            return Err(DatabaseError::InvalidData);
        }
        match (purchase.seller(), seller.as_ref()) {
            (Some(original), Some(refund))
                if refund.guild_id() == original.guild_id()
                    && refund.user_id() == original.user_id()
                    && refund.reason() == LedgerReason::SaleRefund(original.id())
                    && refund.amount()
                        == values::negated(original.amount())
                            .map_err(|_| DatabaseError::InvalidData)? => {}
            (None, None) => {}
            _ => return Err(DatabaseError::InvalidData),
        }
        Ok(RefundResult::new(buyer, seller))
    }
    async fn validate_purchase(
        session: &mut SqliteSession,
        actor: Actor,
        item: &ShopItem,
    ) -> Result<(), RepositoryError> {
        if item.is_deleted() {
            return Err(RepositoryError::ItemDeleted);
        }
        if let ItemTarget::Content(file) = item.target()
            && file.size_bytes() > MAX_CONTENT_BYTES
        {
            return Err(ValidationError::InvalidFileSize.into());
        }
        if item.seller_id() == actor.user_id() && matches!(item.target(), ItemTarget::Content(_)) {
            return Err(RepositoryError::SelfPurchase);
        }
        if session
            .has_purchase(actor.guild_id(), actor.user_id(), item.id())
            .await?
        {
            return Err(RepositoryError::AlreadyPurchased);
        }
        let amount = values::amount(item.details().price().get());
        let buyer_balance = session
            .get_balance(actor.guild_id(), actor.user_id())
            .await?;
        if buyer_balance.get() < amount.get() {
            return Err(RepositoryError::InsufficientBalance);
        }
        values::apply_balance(buyer_balance, values::negated(amount)?)?;
        if matches!(item.target(), ItemTarget::Content(_)) {
            values::apply_balance(
                session
                    .get_balance(actor.guild_id(), item.seller_id())
                    .await?,
                amount,
            )?;
        }
        Ok(())
    }
    fn next_revision(revision: i64) -> Result<i64, RepositoryError> {
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
    ) -> Result<ShopItem, RepositoryError> {
        let item = session
            .find_item(actor.guild_id(), item_id)
            .await?
            .ok_or(RepositoryError::ItemNotFound)?;
        if item.seller_id() != actor.user_id() {
            return Err(RepositoryError::PermissionDenied);
        }
        Ok(item)
    }
}

impl ShopRepository {
    pub(super) fn read_item(row: QueryResult) -> Result<ShopItem, DatabaseError> {
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
            "ROLE" => ItemTarget::Role(values::parse_role_id(&target)?),
            "CONTENT" => {
                let key: String = row.try_get("", "file_key")?;
                let filename: String = row.try_get("", "filename")?;
                let size: i64 = row.try_get("", "size_bytes")?;
                let size = u64::try_from(size).map_err(|_| DatabaseError::InvalidData)?;
                ItemTarget::Content(values::content_file(key, filename, size)?)
            }
            _ => return Err(DatabaseError::InvalidData),
        };
        let details = values::item_details(name, description, values::price(price)?)?;
        values::shop_item(
            values::item_id(id)?,
            values::parse_guild_id(&guild)?,
            values::parse_user_id(&seller)?,
            target,
            details,
            revision,
            deleted.is_some(),
        )
        .map_err(Into::into)
    }
}
