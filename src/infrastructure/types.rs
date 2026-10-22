use crate::domain::*;
pub use crate::domain::{
    ITEM_DESCRIPTION_MAX_CHARS, ITEM_NAME_MAX_CHARS, MoneyError, ValidationError,
};

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
        .and_then(|value| Balance::new(value).ok())
        .ok_or("目標残高が上限を超えています。")
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

macro_rules! id_conversion {
    ($parse:ident, $id:ident, $value:ty) => {
        pub(crate) fn $parse(value: &str) -> Result<$id, ValidationError> {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ValidationError::InvalidId(stringify!($id)));
            }
            $id::new(
                value
                    .parse::<$value>()
                    .map_err(|_| ValidationError::InvalidId(stringify!($id)))?,
            )
        }
    };
}
id_conversion!(parse_guild_id, GuildId, u64);
id_conversion!(parse_user_id, UserId, u64);
id_conversion!(parse_channel_id, ChannelId, u64);
id_conversion!(parse_message_id, MessageId, u64);
id_conversion!(parse_role_id, RoleId, u64);
id_conversion!(parse_item_id, ItemId, i64);
id_conversion!(parse_ledger_entry_id, LedgerEntryId, i64);
