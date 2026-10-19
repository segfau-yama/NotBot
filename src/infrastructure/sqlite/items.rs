use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Statement, raw_sql};

use super::{DatabaseError, SqliteSession};
use crate::domain::{
    Actor, ChannelId, ContentFile, GuildId, ItemDetails, ItemId, ItemTarget, MessageId,
    NewShopItem, Price, RoleId, ShopItem,
};

const CURRENT_ITEM_QUERY: &str = "SELECT i.*, f.key AS file_key, f.filename, f.size_bytes
    FROM current_shop_item AS i
    LEFT JOIN content_file AS f ON i.kind = 'CONTENT' AND f.key = i.target";

impl SqliteSession {
    async fn query_item(&mut self, query: Statement) -> Result<Option<ShopItem>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction
                .query_one_raw(query)
                .await?
                .map(read_item)
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_item(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!("{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.id = ?"),
            [guild_id.to_string().into(), item_id.get().into()],
        ))
        .await
    }

    pub async fn find_content_item(
        &mut self,
        guild_id: GuildId,
        key: &str,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!(
                "{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.kind = 'CONTENT' AND i.target = ?"
            ),
            [guild_id.to_string().into(), key.into()],
        ))
        .await
    }

    pub(super) async fn find_role_item(
        &mut self,
        guild: GuildId,
        role: RoleId,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!(
                "{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.kind = 'ROLE' AND i.target = ?"
            ),
            [guild.to_string().into(), role.to_string().into()],
        ))
        .await
    }

    pub(super) async fn append_role_item(
        &mut self,
        actor: Actor,
        role: RoleId,
        details: ItemDetails,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let guild = actor.guild_id().to_string();
        let seller = actor.user_id().to_string();
        let target = role.to_string();
        let result = async {
            let inserted = transaction.execute_raw(raw_sql!(Sqlite,
                "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES ({guild}, {seller}, 'ROLE', {target})"
            )).await?;
            let id = i64::try_from(inserted.last_insert_id()).map_err(|_| DatabaseError::InvalidData)?;
            let item = ShopItem::try_new(ItemId::try_from(id)?, actor.guild_id(), actor.user_id(), ItemTarget::Role(role), details, 1, false)?;
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_item_revision(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
        revision: i64,
    ) -> Result<Option<ShopItem>, DatabaseError> {
        self.query_item(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT i.id, i.guild_id, i.seller_id, i.kind, i.target,
                    r.revision, r.name, r.description, r.price, r.deleted_at,
                    f.key AS file_key, f.filename, f.size_bytes
             FROM shop_item AS i JOIN shop_item_revision AS r ON r.item_id = i.id
             LEFT JOIN content_file AS f ON i.kind = 'CONTENT' AND f.key = i.target
             WHERE i.guild_id = ? AND i.id = ? AND r.revision = ?",
            [
                guild_id.to_string().into(),
                item_id.get().into(),
                revision.into(),
            ],
        ))
        .await
    }

    pub async fn list_active_items(
        &mut self,
        guild_id: GuildId,
    ) -> Result<Vec<ShopItem>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let result = async {
            transaction.query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("{CURRENT_ITEM_QUERY} WHERE i.guild_id = ? AND i.deleted_at IS NULL ORDER BY i.id"),
                [guild_id.to_string().into()],
            )).await?.into_iter().map(read_item).collect()
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_content_item(
        &mut self,
        actor: Actor,
        item: NewShopItem,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let result = async {
            let ItemTarget::Content(file) = item.target() else {
                return Err(DatabaseError::InvalidData);
            };
            transaction.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO content_file (key, filename, size_bytes) VALUES (?, ?, ?)",
                [file.key().into(), file.filename().into(), (file.size_bytes() as i64).into()],
            )).await?;
            let inserted = transaction.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO shop_item (guild_id, seller_id, kind, target) VALUES (?, ?, 'CONTENT', ?)",
                [actor.guild_id().to_string().into(), actor.user_id().to_string().into(), file.key().into()],
            )).await?;
            let id = i64::try_from(inserted.last_insert_id()).ok()
                .and_then(|id| ItemId::try_from(id).ok()).ok_or(DatabaseError::InvalidData)?;
            let item = ShopItem::try_new(id, actor.guild_id(), actor.user_id(), item.target().clone(), item.details().clone(), 1, false)?;
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }.await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_item_revision(
        &mut self,
        item: ShopItem,
    ) -> Result<ShopItem, DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let result = async {
            insert_revision(&transaction, &item).await?;
            Ok(item)
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn find_publication(
        &mut self,
        guild_id: GuildId,
        item_id: ItemId,
    ) -> Result<Option<(i64, ChannelId, MessageId)>, DatabaseError> {
        let transaction = self.take_transaction()?;
        let guild = guild_id.to_string();
        let item = item_id.get();
        let result = async {
            transaction
                .query_one_raw(raw_sql!(
                    Sqlite,
                    "SELECT p.item_revision, p.channel_id, p.message_id
                 FROM shop_item_publication AS p JOIN shop_item AS i ON i.id = p.item_id
                 WHERE i.guild_id = {guild} AND i.id = {item} ORDER BY p.id DESC LIMIT 1"
                ))
                .await?
                .map(|row| {
                    let revision: i64 = row.try_get("", "item_revision")?;
                    let channel: String = row.try_get("", "channel_id")?;
                    let message: String = row.try_get("", "message_id")?;
                    if revision <= 0 {
                        return Err(DatabaseError::InvalidData);
                    }
                    Ok((revision, channel.parse()?, message.parse()?))
                })
                .transpose()
        }
        .await;
        self.finish_operation(transaction, result).await
    }

    pub async fn append_publication(
        &mut self,
        item: &ShopItem,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> Result<(), DatabaseError> {
        if !self.writable {
            return Err(DatabaseError::ReadOnly);
        }
        let transaction = self.take_transaction()?;
        let item_id = item.id().get();
        let revision = item.revision();
        let channel = channel_id.to_string();
        let message = message_id.to_string();
        let result = async {
            transaction
                .execute_raw(raw_sql!(Sqlite,
                "INSERT INTO shop_item_publication (item_id, item_revision, channel_id, message_id)
                 VALUES ({item_id}, {revision}, {channel}, {message})"
            ))
                .await?;
            Ok(())
        }
        .await;
        self.finish_operation(transaction, result).await
    }
}

async fn insert_revision(
    transaction: &DatabaseTransaction,
    item: &ShopItem,
) -> Result<(), DatabaseError> {
    transaction.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO shop_item_revision (item_id, revision, name, description, price, deleted_at)
         VALUES (?, ?, ?, ?, ?, CASE WHEN ? THEN CURRENT_TIMESTAMP ELSE NULL END)",
        [item.id().get().into(), item.revision().into(), item.details().name().into(),
         item.details().description().into(), item.details().price().get().into(), item.is_deleted().into()],
    )).await?;
    Ok(())
}

fn read_item(row: QueryResult) -> Result<ShopItem, DatabaseError> {
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
        "ROLE" => ItemTarget::Role(target.parse()?),
        "CONTENT" => {
            let key: String = row.try_get("", "file_key")?;
            let filename: String = row.try_get("", "filename")?;
            let size: i64 = row.try_get("", "size_bytes")?;
            let size = u64::try_from(size).map_err(|_| DatabaseError::InvalidData)?;
            ItemTarget::Content(ContentFile::try_new(key, filename, size)?)
        }
        _ => return Err(DatabaseError::InvalidData),
    };
    let details = ItemDetails::try_new(name, description, Price::try_from(price)?)?;
    ShopItem::try_new(
        ItemId::try_from(id)?,
        guild.parse()?,
        seller.parse()?,
        target,
        details,
        revision,
        deleted.is_some(),
    )
    .map_err(Into::into)
}
