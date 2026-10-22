use crate::domain;
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
    pub fn read(&self, value: &str) -> Result<ItemId, ValidationError> {
        values::parse_item_id(value)
    }
    pub(super) fn parse_purchase_id(&self, value: &str) -> Result<LedgerEntryId, ValidationError> {
        values::parse_ledger_entry_id(value)
    }
    pub(super) fn parse_role_id(&self, value: &str) -> Result<RoleId, ValidationError> {
        values::parse_role_id(value)
    }
    pub(super) fn publication_ids(
        &self,
        channel: serenity::all::ChannelId,
        message: serenity::all::MessageId,
    ) -> Result<(ChannelId, MessageId), ValidationError> {
        Ok((
            domain::ChannelId::new(channel.get())?,
            domain::MessageId::new(message.get())?,
        ))
    }
    pub fn create(
        &self,
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
        Ok(domain::ItemDetails::new(
            title,
            description,
            domain::Price::new(amount.get())?,
        )?)
    }

    pub fn update(
        &self,
        id: &str,
        revision: &str,
        title: String,
        description: String,
        price: &str,
    ) -> Result<(ItemId, i64, ItemDetails), Error> {
        let id = self.read(id)?;
        let revision = self.parse_revision(revision)?;
        Ok((id, revision, self.create(title, description, price)?))
    }

    pub fn delete(&self, id: &str, revision: &str) -> Result<(ItemId, i64), Error> {
        Ok((self.read(id)?, self.parse_revision(revision)?))
    }
    pub(super) fn parse_revision(&self, value: &str) -> Result<i64, Error> {
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
