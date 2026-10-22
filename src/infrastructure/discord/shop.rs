use crate::{
    domain::{ChannelId, ItemDetails, ItemId, LedgerEntryId, MessageId, RoleId},
    infrastructure::{Error, ValidationError, types as values},
};
use std::path::PathBuf;

#[derive(Clone)]
pub struct ShopRepository {
    pub(super) directory: PathBuf,
}
impl ShopRepository {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub(super) fn parse_item_id(value: &str) -> Result<ItemId, ValidationError> {
        values::parse_item_id(value)
    }
    pub(super) fn parse_purchase_id(value: &str) -> Result<LedgerEntryId, ValidationError> {
        values::parse_ledger_entry_id(value)
    }
    pub(super) fn parse_role_id(value: &str) -> Result<RoleId, ValidationError> {
        values::parse_role_id(value)
    }
    pub(super) fn publication_ids(
        channel: serenity::all::ChannelId,
        message: serenity::all::MessageId,
    ) -> Result<(ChannelId, MessageId), ValidationError> {
        Ok((
            values::channel_id(channel.get())?,
            values::message_id(message.get())?,
        ))
    }
    pub(super) fn item_details(
        title: String,
        description: String,
        price: &str,
    ) -> Result<ItemDetails, Error> {
        // 共通の文字数検証に加え、DiscordのUTF-16単位の上限も確認する。
        if title.encode_utf16().count() > 256 || description.encode_utf16().count() > 4096 {
            return Err("商品名または説明がDiscordの文字数上限を超えています。".into());
        }
        let amount =
            values::parse_balance(price).map_err(|_| "価格は正のNOT整数で指定してください。")?;
        Ok(values::item_details(
            title,
            description,
            values::price(amount.get())?,
        )?)
    }
    pub(super) fn parse_revision(value: &str) -> Result<i64, Error> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(crate::infrastructure::ValidationError::InvalidRevision.into());
        }
        value
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| crate::infrastructure::ValidationError::InvalidRevision.into())
    }
}
