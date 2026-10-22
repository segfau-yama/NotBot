use crate::domain::*;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    #[error("この操作を行う権限がありません")]
    PermissionDenied,
    #[error("同じ依頼IDが別の操作内容に使われています")]
    RequestConflict,
    #[error("商品が見つかりません")]
    ItemNotFound,
    #[error("削除済みの商品は変更できません")]
    ItemDeleted,
    #[error("商品が変更されています。最新の版を確認してください")]
    RevisionConflict,
    #[error("購入確認の期限が切れています")]
    QuoteExpired,
    #[error("自分の商品は購入できません")]
    SelfPurchase,
    #[error("この商品は購入済みです")]
    AlreadyPurchased,
    #[error("NOT残高が不足しています")]
    InsufficientBalance,
    #[error("自分自身には譲渡できません")]
    SelfTransfer,
    #[error("譲渡額は100・500・1000 NOTのいずれかを指定してください")]
    InvalidTransferAmount,
    #[error("報酬対象の取引理由ではありません")]
    InvalidRewardReason,
    #[error("報酬の受付日時が範囲外です")]
    InvalidRewardTime,
    #[error("購入が見つかりません")]
    PurchaseNotFound,
    #[error("この購入は返金済みです")]
    AlreadyRefunded,
    #[error("{0}")]
    Validation(#[from] ValidationError),
    #[error("{0}")]
    Money(#[from] MoneyError),
    #[error("{0}")]
    Database(#[from] DatabaseError),
}

/// DMへの単一添付で扱う上限。Discordのユーザー／ギルド別の増枠は使わない。
pub const MAX_CONTENT_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("価格は0より大きい整数にしてください")]
    InvalidPrice,
    #[error("残高は0以上にしてください")]
    NegativeBalance,
    #[error("金額の計算結果が整数の範囲を超えています")]
    Overflow,
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

pub const ITEM_NAME_MAX_CHARS: usize = 256;
pub const ITEM_DESCRIPTION_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DatabaseError {
    #[error("データベースを利用できません")]
    Unavailable,
    #[error("データの書き込みが競合しました")]
    Conflict,
    #[error("データの形式または整合性が不正です")]
    InvalidData,
    #[error("読み取り専用セッションには追記できません")]
    ReadOnly,
    #[error("確定結果を確認できません。同じ依頼IDで結果を照合してください")]
    OutcomeUnknown,
}

pub(crate) fn parse_balance(value: &str) -> Result<Balance, &'static str> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("目標残高は0以上の整数で指定してください。");
    }
    value
        .parse::<i64>()
        .ok()
        .and_then(|value| balance(value).ok())
        .ok_or("目標残高が上限を超えています。")
}

pub(crate) fn balance(value: i64) -> Result<Balance, crate::infrastructure::MoneyError> {
    if value >= 0 {
        Ok(Balance::new(value))
    } else {
        Err(crate::infrastructure::MoneyError::NegativeBalance)
    }
}

pub(crate) fn checked_add(
    left: NotAmount,
    right: NotAmount,
) -> Result<NotAmount, crate::infrastructure::MoneyError> {
    left.get()
        .checked_add(right.get())
        .map(NotAmount::new)
        .ok_or(crate::infrastructure::MoneyError::Overflow)
}

pub(crate) fn checked_sub(
    left: NotAmount,
    right: NotAmount,
) -> Result<NotAmount, crate::infrastructure::MoneyError> {
    left.get()
        .checked_sub(right.get())
        .map(NotAmount::new)
        .ok_or(crate::infrastructure::MoneyError::Overflow)
}

pub(crate) fn negated(amount: NotAmount) -> Result<NotAmount, crate::infrastructure::MoneyError> {
    amount
        .get()
        .checked_neg()
        .map(NotAmount::new)
        .ok_or(crate::infrastructure::MoneyError::Overflow)
}

pub(crate) fn apply_balance(
    current: Balance,
    amount: NotAmount,
) -> Result<Balance, crate::infrastructure::MoneyError> {
    balance(checked_add(NotAmount::new(current.get()), amount)?.get())
}

pub(crate) fn ledger_entry(
    id: LedgerEntryId,
    guild_id: GuildId,
    user_id: UserId,
    amount: NotAmount,
    reason: LedgerReason,
    event_key: impl Into<String>,
) -> Result<LedgerEntry, ValidationError> {
    let event_key = event_key.into();
    validate_ledger_values(amount, reason, &event_key)?;
    if match reason {
        LedgerReason::Sale(id) | LedgerReason::Refund(id) | LedgerReason::SaleRefund(id) => {
            Some(id)
        }
        _ => None,
    } == Some(id)
    {
        return Err(ValidationError::InvalidLedgerReference);
    }
    Ok(LedgerEntry::new(
        id, guild_id, user_id, amount, reason, event_key,
    ))
}

pub(crate) fn new_ledger_entry(
    guild_id: GuildId,
    user_id: UserId,
    amount: NotAmount,
    reason: LedgerReason,
    event_key: impl Into<String>,
) -> Result<NewLedgerEntry, ValidationError> {
    let event_key = event_key.into();
    validate_ledger_values(amount, reason, &event_key)?;
    Ok(NewLedgerEntry::new(
        guild_id, user_id, amount, reason, event_key,
    ))
}

pub(crate) fn validate_ledger_values(
    amount: NotAmount,
    reason: LedgerReason,
    event_key: &str,
) -> Result<(), crate::infrastructure::ValidationError> {
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
        return Err(crate::infrastructure::ValidationError::InvalidLedgerAmount);
    }
    Ok(())
}

pub(crate) fn validate_event_key(event_key: &str) -> Result<(), ValidationError> {
    if event_key.trim().is_empty() || event_key.chars().any(char::is_control) {
        return Err(ValidationError::InvalidEventKey);
    }
    Ok(())
}

pub(crate) fn reason_code(reason: LedgerReason) -> &'static str {
    match reason {
        LedgerReason::Message => "MESSAGE",
        LedgerReason::Reaction => "REACTION",
        LedgerReason::ReactionReceived => "REACTION_RECEIVED",
        LedgerReason::ReplyReceived => "REPLY_RECEIVED",
        LedgerReason::TipSent => "TIP_SENT",
        LedgerReason::TipReceived => "TIP_RECEIVED",
        LedgerReason::Timeout => "TIMEOUT",
        LedgerReason::Purchase(_) => "PURCHASE",
        LedgerReason::Sale(_) => "SALE",
        LedgerReason::Refund(_) => "REFUND",
        LedgerReason::SaleRefund(_) => "SALE_REFUND",
        LedgerReason::Adjustment => "ADJUSTMENT",
    }
}

pub(crate) fn amount(value: i64) -> NotAmount {
    NotAmount::new(value)
}

pub(crate) fn actor(guild_id: GuildId, user_id: UserId, can_manage_not: bool) -> Actor {
    Actor::new(guild_id, user_id, can_manage_not)
}

pub(crate) fn price(value: i64) -> Result<crate::domain::Price, crate::infrastructure::MoneyError> {
    if value > 0 {
        Ok(crate::domain::Price::new(value))
    } else {
        Err(crate::infrastructure::MoneyError::InvalidPrice)
    }
}

pub(crate) fn content_file(
    key: impl Into<String>,
    filename: impl Into<String>,
    size_bytes: u64,
) -> Result<ContentFile, ValidationError> {
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
    Ok(ContentFile::new(key, filename, size_bytes))
}

pub(crate) fn item_details(
    name: impl Into<String>,
    description: impl Into<String>,
    price: Price,
) -> Result<ItemDetails, ValidationError> {
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
    Ok(ItemDetails::new(name, description, price))
}

pub(crate) fn shop_item(
    id: ItemId,
    guild_id: GuildId,
    seller_id: UserId,
    target: ItemTarget,
    details: ItemDetails,
    revision: i64,
    deleted: bool,
) -> Result<ShopItem, ValidationError> {
    if revision <= 0 {
        return Err(ValidationError::InvalidRevision);
    }
    Ok(ShopItem::new(
        id, guild_id, seller_id, target, details, revision, deleted,
    ))
}

pub(crate) fn new_item(details: ItemDetails, target: ItemTarget) -> NewShopItem {
    NewShopItem::new(details, target)
}

macro_rules! id_conversion {
    ($make:ident, $parse:ident, $id:ident, $value:ty) => {
        pub(crate) fn $make(value: $value) -> Result<$id, ValidationError> {
            if value > 0 {
                Ok($id::new(value))
            } else {
                Err(ValidationError::InvalidId(stringify!($id)))
            }
        }
        pub(crate) fn $parse(value: &str) -> Result<$id, ValidationError> {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ValidationError::InvalidId(stringify!($id)));
            }
            $make(
                value
                    .parse::<$value>()
                    .map_err(|_| ValidationError::InvalidId(stringify!($id)))?,
            )
        }
        impl std::fmt::Display for $id {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.get().fmt(f)
            }
        }
    };
}
id_conversion!(guild_id, parse_guild_id, GuildId, u64);
id_conversion!(user_id, parse_user_id, UserId, u64);
id_conversion!(channel_id, parse_channel_id, ChannelId, u64);
id_conversion!(message_id, parse_message_id, MessageId, u64);
id_conversion!(role_id, parse_role_id, RoleId, u64);
id_conversion!(item_id, parse_item_id, ItemId, i64);
id_conversion!(ledger_entry_id, parse_ledger_entry_id, LedgerEntryId, i64);
