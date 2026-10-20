use std::{fmt, str::FromStr, time::SystemTime};

pub const ITEM_NAME_MAX_CHARS: usize = 256;
pub const ITEM_DESCRIPTION_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("価格は0より大きい整数にしてください")]
    InvalidPrice,
    #[error("残高は0以上にしてください")]
    NegativeBalance,
    #[error("金額の計算結果が整数の範囲を超えています")]
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price(i64);

impl TryFrom<i64> for Price {
    type Error = MoneyError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value > 0 {
            Ok(Self(value))
        } else {
            Err(MoneyError::InvalidPrice)
        }
    }
}

impl Price {
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

    pub fn get(self) -> i64 {
        self.0
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balance(i64);

impl TryFrom<i64> for Balance {
    type Error = MoneyError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value >= 0 {
            Ok(Self(value))
        } else {
            Err(MoneyError::NegativeBalance)
        }
    }
}

impl Balance {
    pub fn get(self) -> i64 {
        self.0
    }

    pub fn apply(self, amount: NotAmount) -> Result<Self, MoneyError> {
        let value = self.0.checked_add(amount.0).ok_or(MoneyError::Overflow)?;
        Self::try_from(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0} は範囲内の正の整数にしてください")]
    InvalidId(&'static str),
    #[error("商品名が空、長すぎる、または制御文字を含んでいます")]
    InvalidItemName,
    #[error("商品説明が長すぎる、または不正な制御文字を含んでいます")]
    InvalidDescription,
    #[error("ファイルの保存キーが不正です")]
    InvalidFileKey,
    #[error("ファイル名が不正です")]
    InvalidFileName,
    #[error("ファイルサイズが不正です")]
    InvalidFileSize,
    #[error("商品版番号は正の整数にしてください")]
    InvalidRevision,
    #[error("購入確認の期限が日時の範囲を超えています")]
    InvalidExpiration,
    #[error("イベントキーが空、または制御文字を含んでいます")]
    InvalidEventKey,
    #[error("取引理由と増減額の符号が一致していません")]
    InvalidLedgerAmount,
    #[error("取引自身を取消元や購入元に指定できません")]
    InvalidLedgerReference,
}

// 同じ検証を持つIDだけを定義する。用途の違うIDを相互に渡せないようにする。
macro_rules! positive_id {
    ($name:ident, $value:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name($value);

        impl TryFrom<$value> for $name {
            type Error = ValidationError;

            fn try_from(value: $value) -> Result<Self, Self::Error> {
                if value > 0 {
                    Ok(Self(value))
                } else {
                    Err(ValidationError::InvalidId(stringify!($name)))
                }
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(ValidationError::InvalidId(stringify!($name)));
                }
                let value = value
                    .parse::<$value>()
                    .map_err(|_| ValidationError::InvalidId(stringify!($name)))?;
                Self::try_from(value)
            }
        }

        impl $name {
            pub fn get(self) -> $value {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

positive_id!(GuildId, u64);
positive_id!(UserId, u64);
positive_id!(ChannelId, u64);
positive_id!(MessageId, u64);
positive_id!(RoleId, u64);
positive_id!(ItemId, i64);
positive_id!(LedgerEntryId, i64);

/// ファイルのメタデータ。生成はファイルの存在やDiscordの配送上限を保証しない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFile {
    key: String,
    filename: String,
    size_bytes: u64,
}

impl ContentFile {
    pub fn try_new(
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

/// 商品版ごとに変わる値。出品入力と保存済み商品で同じ検証を使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetails {
    name: String,
    description: String,
    price: Price,
}

impl ItemDetails {
    pub fn try_new(
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
    pub fn try_new(
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

/// 固定のマスタコードに対応する業務上の理由と参照先。
/// 参照先の実在・所属・元取引との金額の一致はユースケースで照合する。
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

impl LedgerReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Message => "MESSAGE",
            Self::Reaction => "REACTION",
            Self::ReactionReceived => "REACTION_RECEIVED",
            Self::ReplyReceived => "REPLY_RECEIVED",
            Self::TipSent => "TIP_SENT",
            Self::TipReceived => "TIP_RECEIVED",
            Self::Timeout => "TIMEOUT",
            Self::Purchase(_) => "PURCHASE",
            Self::Sale(_) => "SALE",
            Self::Refund(_) => "REFUND",
            Self::SaleRefund(_) => "SALE_REFUND",
            Self::Adjustment => "ADJUSTMENT",
        }
    }

    fn accepts_amount(self, amount: NotAmount) -> bool {
        match self {
            Self::Message
            | Self::Reaction
            | Self::ReactionReceived
            | Self::ReplyReceived
            | Self::TipReceived
            | Self::Sale(_)
            | Self::Refund(_) => amount.get() > 0,
            Self::TipSent | Self::Purchase(_) | Self::SaleRefund(_) => amount.get() < 0,
            Self::Timeout => amount.get() <= 0,
            Self::Adjustment => true,
        }
    }

    fn source_id(self) -> Option<LedgerEntryId> {
        match self {
            Self::Sale(id) | Self::Refund(id) | Self::SaleRefund(id) => Some(id),
            _ => None,
        }
    }
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
    pub fn try_new(
        id: LedgerEntryId,
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let event_key = event_key.into();
        validate_ledger_values(amount, reason, &event_key)?;
        if reason.source_id() == Some(id) {
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

pub(crate) fn validate_ledger_values(
    amount: NotAmount,
    reason: LedgerReason,
    event_key: &str,
) -> Result<(), ValidationError> {
    validate_event_key(event_key)?;
    if !reason.accepts_amount(amount) {
        return Err(ValidationError::InvalidLedgerAmount);
    }
    Ok(())
}

pub(crate) fn validate_event_key(event_key: &str) -> Result<(), ValidationError> {
    if event_key.trim().is_empty() || event_key.chars().any(char::is_control) {
        return Err(ValidationError::InvalidEventKey);
    }
    Ok(())
}

/// Discord側で対象ギルドの所属と権限を確認した実行者。
/// コンストラクタ自体は認証や権限の照会を行わない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    guild_id: GuildId,
    user_id: UserId,
    can_manage_not: bool,
}

impl Actor {
    pub fn new(guild_id: GuildId, user_id: UserId, can_manage_not: bool) -> Self {
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

/// ギルドと出品者は、商品を作成する操作処理でActorから決定する。
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
    pub fn try_new(
        guild_id: GuildId,
        user_id: UserId,
        amount: NotAmount,
        reason: LedgerReason,
        event_key: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let event_key = event_key.into();
        validate_ledger_values(amount, reason, &event_key)?;
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

/// サーバーが発行した購入確認。外部から価格・版・期限を差し替えられない。
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

/// 商品は現在の状態。実際の支払額はbuyerの負の増減額から取得する。
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

/// 返金履歴。元の購入に売上がある場合だけ売上取消も一括確定する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeResult {
    buyer: LedgerEntry,
    seller: Option<LedgerEntry>,
}

impl TradeResult {
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
