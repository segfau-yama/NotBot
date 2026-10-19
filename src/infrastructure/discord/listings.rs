use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serenity::all::*;

use super::{Data, content, events::public_error_message, services};
use crate::{
    domain::{Actor, ItemDetails, ItemId, ItemTarget, Price, ShopItem},
    infrastructure::{Error, UseCaseError},
};

pub(super) fn download_client() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "添付取得の準備に失敗しました。")?)
}

pub(super) struct ListingDraft {
    id: String,
    actor: Actor,
    details: ItemDetails,
    attachment: Attachment,
    expires_at: Instant,
}

impl ListingDraft {
    pub(super) fn new(
        id: String,
        actor: Actor,
        title: String,
        description: String,
        price: &str,
        attachment: Attachment,
    ) -> Result<Self, Error> {
        let details = item_details(title, description, price)?;
        content::validate_attachment(&attachment)?;
        Ok(Self {
            id,
            actor,
            details,
            attachment,
            expires_at: Instant::now() + Duration::from_secs(300),
        })
    }

    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub(super) fn preview(&self, guild_name: &str) -> poise::CreateReply {
        poise::CreateReply::default()
            .content("出品内容を確認してください。有効期限は5分です。新しい下書きを作ると以前の下書きは失効します。")
            .embed(item_embed(&self.details, self.actor, &self.attachment.filename, self.attachment.size.into(), guild_name))
            .components(vec![CreateActionRow::Buttons(vec![
                CreateButton::new(format!("content:draft:{}:publish", self.id)).label("出品する").style(ButtonStyle::Success),
                CreateButton::new(format!("content:draft:{}:cancel", self.id)).label("キャンセル").style(ButtonStyle::Secondary),
            ])]).allowed_mentions(CreateAllowedMentions::new()).ephemeral(true)
    }
}

pub(super) fn item_details(
    title: String,
    description: String,
    price: &str,
) -> Result<ItemDetails, Error> {
    // DiscordはUTF-16単位で上限を判定するため、ドメインの文字数検証に加えて確認する。
    if title.encode_utf16().count() > 256 || description.encode_utf16().count() > 4096 {
        return Err("商品名または説明がDiscordの文字数上限を超えています。".into());
    }
    let amount =
        services::parse_balance(price).map_err(|_| "価格は正のNOT整数で指定してください。")?;
    Ok(ItemDetails::try_new(
        title,
        description,
        Price::try_from(amount.get())?,
    )?)
}

fn take_draft(
    drafts: &mut HashMap<u64, ListingDraft>,
    user: u64,
    id: &str,
) -> Result<ListingDraft, Error> {
    if !drafts
        .get(&user)
        .is_some_and(|draft| draft.id == id && !draft.expired())
    {
        return Err("下書きが失効しています。/content add から入力し直してください。".into());
    }
    Ok(drafts.remove(&user).expect("checked draft"))
}

pub(super) async fn remember_draft(
    data: &Data,
    draft: ListingDraft,
    guild_name: &str,
) -> poise::CreateReply {
    let reply = draft.preview(guild_name);
    let mut drafts = data.drafts.lock().await;
    drafts.retain(|_, draft| !draft.expired());
    drafts.insert(draft.actor.user_id().get(), draft);
    reply
}

pub(super) async fn selling_channel(
    http: &Http,
    data: &Data,
    actor: Actor,
) -> Result<(ChannelId, String), Error> {
    let channel_id = data
        .shop_channels
        .get(&actor.guild_id())
        .ok_or("対象サーバーの販売チャンネルが設定されていません。")?;
    let channel = http
        .get_channel(ChannelId::new(channel_id.get()))
        .await
        .map_err(|_| "販売チャンネルを確認できませんでした。")?
        .guild()
        .filter(|channel| {
            channel.guild_id.get() == actor.guild_id().get()
                && matches!(channel.kind, ChannelType::Text | ChannelType::News)
        })
        .ok_or("販売チャンネルは対象サーバーのテキストチャンネルを指定してください。")?;
    let guild = http
        .get_guild(channel.guild_id)
        .await
        .map_err(|_| "対象サーバーを確認できませんでした。")?;
    let bot = http
        .get_current_user()
        .await
        .map_err(|_| "Botを確認できませんでした。")?;
    let bot = http
        .get_member(channel.guild_id, bot.id)
        .await
        .map_err(|_| "Botの所属を確認できませんでした。")?;
    let permissions = guild.user_permissions_in(&channel, &bot);
    if !permissions
        .contains(Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES | Permissions::EMBED_LINKS)
    {
        return Err(
            "Botに販売チャンネルの閲覧・メッセージ送信・埋め込みリンク権限が必要です。".into(),
        );
    }
    Ok((channel.id, guild.name))
}

fn item_embed(
    details: &ItemDetails,
    actor: Actor,
    filename: &str,
    size: u64,
    guild_name: &str,
) -> CreateEmbed {
    CreateEmbed::new()
        .title(details.name())
        .description(details.description())
        .field("価格", format!("{} NOT", details.price().get()), true)
        .field("出品者", format!("<@{}>", actor.user_id()), true)
        .field("ファイル", format!("{}（{} bytes）", filename, size), false)
        .field("対象サーバー", guild_name, false)
}

fn public_message(item: &ShopItem, guild_name: &str) -> (CreateEmbed, Vec<CreateActionRow>) {
    let actor = Actor::new(item.guild_id(), item.seller_id(), false);
    let embed = match item.target() {
        ItemTarget::Content(file) => item_embed(
            item.details(),
            actor,
            file.filename(),
            file.size_bytes(),
            guild_name,
        ),
        ItemTarget::Role(role) => CreateEmbed::new()
            .title(item.details().name())
            .description(item.details().description())
            .field(
                "価格",
                format!("{} NOT", item.details().price().get()),
                true,
            )
            .field("出品者", format!("<@{}>", item.seller_id()), true)
            .field("ロール", format!("<@&{role}>"), false)
            .field("対象サーバー", guild_name, false),
    }
    .footer(CreateEmbedFooter::new(format!(
        "商品ID: {} / 版: {}",
        item.id(),
        item.revision()
    )));
    (
        embed,
        vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("content:buy:{}:{}", item.guild_id(), item.id()))
                .label(if item.is_deleted() {
                    "販売終了"
                } else {
                    "購入する"
                })
                .style(ButtonStyle::Primary)
                .disabled(item.is_deleted()),
        ])],
    )
}

#[derive(Debug, thiserror::Error)]
#[error(
    "PublicationPending: 商品ID {item_id} は保存済みですが、公開を確認できません。再投稿してください。"
)]
struct PublicationPending {
    item_id: ItemId,
}

async fn publish(
    http: &Http,
    data: &Data,
    actor: Actor,
    item_id: ItemId,
) -> Result<MessageId, Error> {
    let lock = data.publication_lock(item_id).await;
    let _publication = lock.lock().await;
    let item = match async {
        let mut session = data.database.begin_read().await?;
        let item = session.find_item(actor.guild_id(), item_id).await?;
        session.commit().await?;
        item.ok_or(UseCaseError::ItemNotFound)
    }
    .await
    {
        Err(UseCaseError::Database(_)) => return Err(PublicationPending { item_id }.into()),
        result => result?,
    };
    if item.seller_id() != actor.user_id() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let result: Result<MessageId, Error> = async {
        let (channel, guild_name) = selling_channel(http, data, actor).await?;
        let mut session = data.database.begin_read().await?;
        let existing = session.find_publication(actor.guild_id(), item_id).await?;
        session.commit().await?;
        let (embed, buttons) = public_message(&item, &guild_name);
        let mut published = None;
        if let Some((_, saved_channel, message)) = existing
            && saved_channel.get() == channel.get()
        {
            match channel
                .edit_message(
                    http,
                    MessageId::new(message.get()),
                    EditMessage::new()
                        .embed(embed.clone())
                        .components(buttons.clone())
                        .allowed_mentions(CreateAllowedMentions::new()),
                )
                .await
            {
                Ok(message) => published = Some(message.id),
                Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
                    response,
                ))) if response.error.code == 10008 => {}
                Err(error) => return Err(error.into()),
            }
        }
        let message = match published {
            Some(message) => message,
            None => {
                channel
                    .send_message(
                        http,
                        CreateMessage::new()
                            .embed(embed)
                            .components(buttons)
                            .allowed_mentions(CreateAllowedMentions::new()),
                    )
                    .await?
                    .id
            }
        };
        if existing.is_some_and(|(revision, saved_channel, saved_message)| {
            revision == item.revision()
                && saved_channel.get() == channel.get()
                && saved_message.get() == message.get()
        }) {
            return Ok(message);
        }
        let mut session = data.database.begin_write().await?;
        session
            .append_publication(
                &item,
                crate::domain::ChannelId::try_from(channel.get())?,
                crate::domain::MessageId::try_from(message.get())?,
            )
            .await?;
        session.commit().await?;
        println!(
            "content publication guild={} item={} revision={} deleted={}",
            actor.guild_id(),
            item_id,
            item.revision(),
            item.is_deleted()
        );
        Ok(message)
    }
    .await;
    result.map_err(|_| Box::new(PublicationPending { item_id }) as Error)
}

pub(super) async fn publication_status(
    http: &Http,
    data: &Data,
    actor: Actor,
    item_id: ItemId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    match publish(http, data, actor, item_id).await {
        Ok(message) => {
            let channel = data
                .shop_channels
                .get(&actor.guild_id())
                .ok_or("販売チャンネルが設定されていません。")?;
            Ok((
                format!(
                    "公開表示へ反映しました。商品ID: {item_id}\nhttps://discord.com/channels/{}/{}/{}",
                    actor.guild_id(),
                    channel,
                    message
                ),
                vec![],
            ))
        }
        Err(error) if error.is::<PublicationPending>() => {
            eprintln!(
                "content publication_pending guild={} item={item_id}",
                actor.guild_id()
            );
            Ok((
                error.to_string(),
                vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(format!(
                        "content:republish:{}:{}",
                        actor.guild_id(),
                        item_id
                    ))
                    .label("再投稿する")
                    .style(ButtonStyle::Primary),
                ])],
            ))
        }
        Err(error) => Err(error),
    }
}

async fn submit(data: &Data, actor: Actor, draft: &ListingDraft) -> Result<ShopItem, Error> {
    if actor.user_id() != draft.actor.user_id() || actor.guild_id() != draft.actor.guild_id() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let bytes = content::download_attachment(&data.download, &draft.attachment).await?;
    let directory = data.content_dir.clone();
    let id = draft.id.clone();
    let filename = draft.attachment.filename.clone();
    let stored = tokio::task::spawn_blocking(move || {
        content::store_content(&directory, actor, &id, &filename, bytes.as_slice())
    })
    .await
    .map_err(|_| "ファイルの保存処理に失敗しました。")??;
    Ok(data
        .shop
        .create(actor, draft.details.clone(), stored)
        .await?)
}

pub(super) async fn handle_action(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    if component.guild_id.is_some() {
        return Err("BotとのDMから操作してください。".into());
    }
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    match parts.as_slice() {
        ["content", "republish", guild, item] => {
            let guild: crate::domain::GuildId = guild.parse()?;
            let item = item.parse()?;
            let actor =
                services::authenticate(http, GuildId::new(guild.get()), component.user.id).await?;
            publication_status(http, data, actor, item).await
        }
        ["content", "draft", id, action @ ("publish" | "cancel")] => {
            let draft = take_draft(&mut *data.drafts.lock().await, component.user.id.get(), id)?;
            if *action == "cancel" {
                return Ok(("出品をキャンセルしました。".into(), vec![]));
            }
            let result = async {
                let actor = services::authenticate(
                    http,
                    GuildId::new(draft.actor.guild_id().get()),
                    component.user.id,
                )
                .await?;
                submit(data, actor, &draft).await
            }
            .await;
            match result {
                Ok(item) => publication_status(http, data, draft.actor, item.id()).await,
                Err(error) => {
                    if !draft.expired() {
                        data.drafts
                            .lock()
                            .await
                            .entry(component.user.id.get())
                            .or_insert(draft);
                    }
                    Ok((
                        format!(
                            "{}\n同じプレビューの「出品する」から再試行できます。",
                            public_error_message(&error)
                        ),
                        vec![],
                    ))
                }
            }
        }
        _ => Err("出品操作が不正です。".into()),
    }
}

#[cfg(test)]
mod tests;
