//! 外部の形式に依存しない業務データ、不変条件、金額計算。
use std::time::{Duration, SystemTime};

pub const ITEM_NAME_MAX_CHARS: usize = 256;
pub const ITEM_DESCRIPTION_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneyError {
    InvalidPrice,
    NegativeBalance,
    Overflow,
}

impl std::fmt::Display for MoneyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPrice => "価格は0より大きい整数にしてください",
            Self::NegativeBalance => "残高は0以上にしてください",
            Self::Overflow => "金額の計算結果が整数の範囲を超えています",
        })
    }
}

impl std::error::Error for MoneyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    InvalidId(&'static str),
    InvalidItemName,
    InvalidDescription,
    InvalidFileKey,
    InvalidFileName,
    InvalidFileSize,
    InvalidRevision,
    InvalidExpiration,
    InvalidEventKey,
    InvalidLedgerAmount,
    InvalidLedgerReference,
    InvalidPurchaseResult,
    InvalidRefundResult,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidId(name) => return write!(f, "{name} は範囲内の正の整数にしてください"),
            Self::InvalidItemName => "商品名が空、長すぎる、または制御文字を含んでいます",
            Self::InvalidDescription => "商品説明が長すぎる、または不正な制御文字を含んでいます",
            Self::InvalidFileKey => "ファイルの保存キーが不正です",
            Self::InvalidFileName => "ファイル名が不正です",
            Self::InvalidFileSize => "ファイルサイズが不正です",
            Self::InvalidRevision => "商品版番号は正の整数にしてください",
            Self::InvalidExpiration => "購入確認の期限が日時の範囲を超えています",
            Self::InvalidEventKey => "イベントキーが空、または制御文字を含んでいます",
            Self::InvalidLedgerAmount => "取引理由と増減額の符号が一致していません",
            Self::InvalidLedgerReference => "取引自身を取消元や購入元に指定できません",
            Self::InvalidPurchaseResult => "商品と購入・売上の整合性が不正です",
            Self::InvalidRefundResult => "購入と返金の整合性が不正です",
        })
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price(i64);

impl Price {
    pub fn new(value: i64) -> Result<Self, MoneyError> {
        if value <= 0 {
            return Err(MoneyError::InvalidPrice);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAmount(i64);

impl NotAmount {
    pub fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn negated(self) -> Result<Self, MoneyError> {
        self.0.checked_neg().map(Self).ok_or(MoneyError::Overflow)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balance(i64);

impl Balance {
    pub fn new(value: i64) -> Result<Self, MoneyError> {
        if value < 0 {
            return Err(MoneyError::NegativeBalance);
        }
        Ok(Self(value))
    }

    pub fn apply(self, amount: NotAmount) -> Result<Self, MoneyError> {
        Self::new(NotAmount::new(self.0).checked_add(amount)?.get())
    }

    pub fn after_purchase(self, price: Price) -> Result<Self, MoneyError> {
        self.apply(NotAmount::new(price.get()).negated()?)
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
            pub fn new(value: $value) -> Result<Self, ValidationError> {
                if value <= 0 {
                    return Err(ValidationError::InvalidId(stringify!($name)));
                }
                Ok(Self(value))
            }

            pub fn get(self) -> $value {
                self.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
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
    pub fn new(
        key: impl Into<String>,
        filename: impl Into<String>,
        size_bytes: u64,
    ) -> Result<Self, ValidationError> {
        let key = key.into();
        let filename = filename.into();
        if key.is_empty()
            || key.len() > 255
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ValidationError::InvalidFileKey);
        }
        // 元のファイル名は配送用の表示値であり、保存パスに使わない。
        if filename.trim().is_empty()
            || matches!(filename.as_str(), "." | "..")
            || filename.contains(['/', '\\'])
            || filename.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidFileName);
        }
        if size_bytes == 0 || i64::try_from(size_bytes).is_err() {
            return Err(ValidationError::InvalidFileSize);
        }
        Ok(Self {
            key,
            filename,
            size_bytes,
        })
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
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        price: Price,
    ) -> Result<Self, ValidationError> {
        let name = name.into();
        let name = name.trim().to_owned();
        let description = description.into();
        if name.is_empty()
            || name.chars().count() > ITEM_NAME_MAX_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidItemName);
        }
        if description.chars().count() > ITEM_DESCRIPTION_MAX_CHARS
            || description
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return Err(ValidationError::InvalidDescription);
        }
        Ok(Self {
            name,
            description,
            price,
        })
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
    pub(crate) fn next_revision(revision: i64) -> Result<i64, ValidationError> {
        if revision <= 0 {
            return Err(ValidationError::InvalidRevision);
        }
        revision
            .checked_add(1)
            .ok_or(ValidationError::InvalidRevision)
    }

    pub fn new(
        id: ItemId,
        guild_id: GuildId,
        seller_id: UserId,
        target: ItemTarget,
        details: ItemDetails,
        revision: i64,
        deleted: bool,
    ) -> Result<Self, ValidationError> {
        if revision <= 0 {
            return Err(ValidationError::InvalidRevision);
        }
        Ok(Self {
            id,
            guild_id,
            seller_id,
            target,
            details,
            revision,
            deleted,
        })
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
    fn validate_values(
        amount: NotAmount,
        reason: LedgerReason,
        event_key: &str,
    ) -> Result<(), ValidationError> {
        validate_event_key(event_key)?;
        let valid = match reason {
            LedgerReason::Message
            | LedgerReason::Reaction
            | LedgerReason::ReactionReceived
            | LedgerReason::ReplyReceived
            | LedgerReason::TipReceived
            | LedgerReason::Sale(_)
            | LedgerReason::Refund(_) => amount.get() > 0,
            LedgerReason::TipSent | LedgerReason::Purchase(_) | LedgerReason::SaleRefund(_) => {
                amount.get() < 0
            }
            LedgerReason::Timeout => amount.get() <= 0,
            LedgerReason::Adjustment => true,
        };
        if !valid {
            return Err(ValidationError::InvalidLedgerAmount);
        }
        Ok(())
    }

    pub fn new(
        id: LedgerEntryId,
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let event_key = event_key.into();
        LedgerEntry::validate_values(amount, reason, &event_key)?;
        if match reason {
            LedgerReason::Sale(id) | LedgerReason::Refund(id) | LedgerReason::SaleRefund(id) => {
                Some(id)
            }
            _ => None,
        } == Some(id)
        {
            return Err(ValidationError::InvalidLedgerReference);
        }
        Ok(Self {
            id,
            guild_id,
            user_id,
            amount,
            reason,
            event_key,
        })
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
    pub fn new(details: ItemDetails, target: ItemTarget) -> Self {
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
    pub fn new(
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let event_key = event_key.into();
        LedgerEntry::validate_values(amount, reason, &event_key)?;
        Ok(Self {
            guild_id,
            user_id,
            amount,
            reason,
            event_key,
        })
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
    pub fn new(user_id: UserId, balance: Balance) -> Self {
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
    pub fn new(
        confirmation_id: String,
        buyer_id: UserId,
        item: ShopItem,
        now: SystemTime,
    ) -> Result<Self, ValidationError> {
        validate_event_key(&confirmation_id)?;
        let expires_at = now
            .checked_add(Duration::from_secs(300))
            .ok_or(ValidationError::InvalidExpiration)?;
        Ok(Self {
            confirmation_id,
            buyer_id,
            item,
            expires_at,
        })
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
    pub fn new(
        item: ShopItem,
        buyer: LedgerEntry,
        seller: Option<LedgerEntry>,
    ) -> Result<Self, ValidationError> {
        if buyer.guild_id() != item.guild_id()
            || buyer.reason() != LedgerReason::Purchase(item.id())
        {
            return Err(ValidationError::InvalidPurchaseResult);
        }
        let amount = buyer
            .amount()
            .negated()
            .map_err(|_| ValidationError::InvalidPurchaseResult)?;
        if amount.get() <= 0
            || (matches!(item.target(), ItemTarget::Content(_))
                && (seller.is_none() || buyer.user_id() == item.seller_id()))
        {
            return Err(ValidationError::InvalidPurchaseResult);
        }
        if let Some(sale) = &seller
            && (sale.guild_id() != buyer.guild_id()
                || sale.user_id() != item.seller_id()
                || sale.reason() != LedgerReason::Sale(buyer.id())
                || sale.amount() != amount)
        {
            return Err(ValidationError::InvalidPurchaseResult);
        }
        Ok(Self {
            item,
            buyer,
            seller,
        })
    }

    pub fn item(&self) -> &ShopItem {
        &self.item
    }
    pub fn buyer(&self) -> &LedgerEntry {
        &self.buyer
    }
    pub fn buyer_actor(&self) -> Actor {
        Actor::new(self.item.guild_id(), self.buyer.user_id(), false)
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
    pub fn new(
        purchase: &PurchaseResult,
        buyer: LedgerEntry,
        seller: Option<LedgerEntry>,
    ) -> Result<Self, ValidationError> {
        if buyer.guild_id() != purchase.buyer().guild_id()
            || buyer.user_id() != purchase.buyer().user_id()
            || buyer.reason() != LedgerReason::Refund(purchase.buyer().id())
            || buyer.amount()
                != purchase
                    .buyer()
                    .amount()
                    .negated()
                    .map_err(|_| ValidationError::InvalidRefundResult)?
        {
            return Err(ValidationError::InvalidRefundResult);
        }
        match (purchase.seller(), seller.as_ref()) {
            (Some(original), Some(refund))
                if refund.guild_id() == original.guild_id()
                    && refund.user_id() == original.user_id()
                    && refund.reason() == LedgerReason::SaleRefund(original.id())
                    && refund.amount()
                        == original
                            .amount()
                            .negated()
                            .map_err(|_| ValidationError::InvalidRefundResult)? => {}
            (None, None) => {}
            _ => return Err(ValidationError::InvalidRefundResult),
        }
        Ok(Self { buyer, seller })
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

pub(crate) fn validate_event_key(event_key: &str) -> Result<(), ValidationError> {
    if event_key.trim().is_empty() || event_key.chars().any(char::is_control) {
        return Err(ValidationError::InvalidEventKey);
    }
    Ok(())
}
