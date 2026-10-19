use crate::domain::{MoneyError, ValidationError};

use super::sqlite::DatabaseError;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum UseCaseError {
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
