//! Repositoryで変換・検証済みの値を保持する。フィールドは非公開で、読み取り操作だけを公開する。
//! crate内の生成メソッドは受け取った値を格納し、変換・検証・計算は行わない。
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price(i64);

impl Price {
    pub(crate) fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAmount(i64);

impl NotAmount {
    pub(crate) fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balance(i64);

impl Balance {
    pub(crate) fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

macro_rules! id_type {
    ($name:ident, $value:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name($value);
        impl $name {
            pub(crate) fn new(value: $value) -> Self {
                Self(value)
            }

            pub fn get(self) -> $value {
                self.0
            }
        }
    };
}

id_type!(GuildId, u64);
id_type!(UserId, u64);
id_type!(ChannelId, u64);
id_type!(MessageId, u64);
id_type!(RoleId, u64);
id_type!(ItemId, i64);
id_type!(LedgerEntryId, i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFile {
    key: String,
    filename: String,
    size_bytes: u64,
}

impl ContentFile {
    pub(crate) fn new(key: String, filename: String, size_bytes: u64) -> Self {
        Self {
            key,
            filename,
            size_bytes,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetails {
    name: String,
    description: String,
    price: Price,
}

impl ItemDetails {
    pub(crate) fn new(name: String, description: String, price: Price) -> Self {
        Self {
            name,
            description,
            price,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn price(&self) -> Price {
        self.price
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemTarget {
    Role(RoleId),
    Content(ContentFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopItem {
    id: ItemId,
    guild_id: GuildId,
    seller_id: UserId,
    target: ItemTarget,
    details: ItemDetails,
    revision: i64,
    deleted: bool,
}

impl ShopItem {
    pub(crate) fn new(
        id: ItemId,
        guild_id: GuildId,
        seller_id: UserId,
        target: ItemTarget,
        details: ItemDetails,
        revision: i64,
        deleted: bool,
    ) -> Self {
        Self {
            id,
            guild_id,
            seller_id,
            target,
            details,
            revision,
            deleted,
        }
    }

    pub fn id(&self) -> ItemId {
        self.id
    }

    pub fn guild_id(&self) -> GuildId {
        self.guild_id
    }

    pub fn seller_id(&self) -> UserId {
        self.seller_id
    }

    pub fn target(&self) -> &ItemTarget {
        &self.target
    }

    pub fn details(&self) -> &ItemDetails {
        &self.details
    }

    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerReason {
    Message,
    Reaction,
    ReactionReceived,
    ReplyReceived,
    TipSent,
    TipReceived,
    Timeout,
    Purchase(ItemId),
    Sale(LedgerEntryId),
    Refund(LedgerEntryId),
    SaleRefund(LedgerEntryId),
    Adjustment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    id: LedgerEntryId,
    guild_id: GuildId,
    user_id: UserId,
    amount: NotAmount,
    reason: LedgerReason,
    event_key: String,
}

impl LedgerEntry {
    pub(crate) fn new(
        id: LedgerEntryId,
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: String,
    ) -> Self {
        Self {
            id,
            guild_id,
            user_id,
            amount,
            reason,
            event_key,
        }
    }

    pub fn id(&self) -> LedgerEntryId {
        self.id
    }

    pub fn guild_id(&self) -> GuildId {
        self.guild_id
    }

    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    pub fn amount(&self) -> NotAmount {
        self.amount
    }

    pub fn reason(&self) -> LedgerReason {
        self.reason
    }

    pub fn event_key(&self) -> &str {
        &self.event_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    guild_id: GuildId,
    user_id: UserId,
    can_manage_not: bool,
}

impl Actor {
    pub(crate) fn new(guild_id: GuildId, user_id: UserId, can_manage_not: bool) -> Self {
        Self {
            guild_id,
            user_id,
            can_manage_not,
        }
    }

    pub fn guild_id(self) -> GuildId {
        self.guild_id
    }

    pub fn user_id(self) -> UserId {
        self.user_id
    }

    pub fn can_manage_not(self) -> bool {
        self.can_manage_not
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewShopItem {
    details: ItemDetails,
    target: ItemTarget,
}

impl NewShopItem {
    pub(crate) fn new(details: ItemDetails, target: ItemTarget) -> Self {
        Self { details, target }
    }

    pub fn details(&self) -> &ItemDetails {
        &self.details
    }

    pub fn target(&self) -> &ItemTarget {
        &self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLedgerEntry {
    guild_id: GuildId,
    user_id: UserId,
    amount: NotAmount,
    reason: LedgerReason,
    event_key: String,
}

impl NewLedgerEntry {
    pub(crate) fn new(
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: String,
    ) -> Self {
        Self {
            guild_id,
            user_id,
            amount,
            reason,
            event_key,
        }
    }

    pub fn guild_id(&self) -> GuildId {
        self.guild_id
    }

    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    pub fn amount(&self) -> NotAmount {
        self.amount
    }

    pub fn reason(&self) -> LedgerReason {
        self.reason
    }

    pub fn event_key(&self) -> &str {
        &self.event_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserBalance {
    user_id: UserId,
    balance: Balance,
}

impl UserBalance {
    pub(crate) fn new(user_id: UserId, balance: Balance) -> Self {
        Self { user_id, balance }
    }

    pub fn user_id(self) -> UserId {
        self.user_id
    }

    pub fn balance(self) -> Balance {
        self.balance
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseQuote {
    confirmation_id: String,
    buyer_id: UserId,
    item: ShopItem,
    expires_at: SystemTime,
}

impl PurchaseQuote {
    pub(crate) fn new(
        confirmation_id: String,
        buyer_id: UserId,
        item: ShopItem,
        expires_at: SystemTime,
    ) -> Self {
        Self {
            confirmation_id,
            buyer_id,
            item,
            expires_at,
        }
    }

    pub fn id(&self) -> &str {
        &self.confirmation_id
    }
    pub fn buyer_id(&self) -> UserId {
        self.buyer_id
    }
    pub fn item(&self) -> &ShopItem {
        &self.item
    }
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseResult {
    item: ShopItem,
    buyer: LedgerEntry,
    seller: Option<LedgerEntry>,
}

impl PurchaseResult {
    pub(crate) fn new(item: ShopItem, buyer: LedgerEntry, seller: Option<LedgerEntry>) -> Self {
        Self {
            item,
            buyer,
            seller,
        }
    }

    pub fn item(&self) -> &ShopItem {
        &self.item
    }
    pub fn buyer(&self) -> &LedgerEntry {
        &self.buyer
    }
    /// 新しいロール購入には売上がない。既存の売上付き購入は保持する。
    pub fn seller(&self) -> Option<&LedgerEntry> {
        self.seller.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundResult {
    buyer: LedgerEntry,
    seller: Option<LedgerEntry>,
}

impl RefundResult {
    pub(crate) fn new(buyer: LedgerEntry, seller: Option<LedgerEntry>) -> Self {
        Self { buyer, seller }
    }

    pub fn buyer(&self) -> &LedgerEntry {
        &self.buyer
    }
    pub fn seller(&self) -> Option<&LedgerEntry> {
        self.seller.as_ref()
    }
}

/// 永続保存の完了を表す。外側の保存処理だけが生成し、未保存のメタデータと区別する。
#[derive(Debug, Clone)]
pub struct StoredContent {
    actor: Actor,
    file: ContentFile,
}

impl StoredContent {
    pub fn actor(&self) -> Actor {
        self.actor
    }

    pub fn into_file(self) -> ContentFile {
        self.file
    }

    pub(crate) fn new(actor: Actor, file: ContentFile) -> Self {
        Self { actor, file }
    }

    pub fn file(&self) -> &ContentFile {
        &self.file
    }
}
