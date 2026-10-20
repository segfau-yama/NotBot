use super::{Context, Data, content, events::public_error_message};
use crate::{
    domain::{
        Actor, Balance, GuildId as DomainGuildId, ItemDetails, ItemId, ItemTarget, LedgerEntryId,
        NotAmount, Price, ShopItem, UserId as DomainUserId,
    },
    infrastructure::{Error, UseCaseError},
};
use serenity::all::*;
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};
use tokio::time::timeout_at;

pub(super) async fn actor(ctx: Context<'_>, supplied: Option<&str>) -> Result<Actor, Error> {
    let guild_id = target_guild(ctx.guild_id(), supplied)?;
    authenticate(ctx.http(), guild_id, ctx.author().id).await
}

pub(super) async fn guild_name(http: &Http, guild: DomainGuildId) -> Result<String, Error> {
    Ok(http.get_guild(GuildId::new(guild.get())).await?.name)
}

pub(super) async fn adjustment_target(
    http: &Http,
    actor: Actor,
    user: UserId,
) -> Result<DomainUserId, Error> {
    if !actor.can_manage_not() {
        return Err("この操作には対象サーバーの管理者権限が必要です。".into());
    }
    member_target(http, actor, user).await
}

pub(super) async fn member_target(
    http: &Http,
    actor: Actor,
    user: UserId,
) -> Result<DomainUserId, Error> {
    let member = http
        .get_member(GuildId::new(actor.guild_id().get()), user)
        .await
        .map_err(|_| "対象ユーザーの所属を確認できませんでした。")?;
    if member.user.id != user || member.guild_id.get() != actor.guild_id().get() {
        return Err("対象ユーザーの所属を確認できませんでした。".into());
    }
    Ok(DomainUserId::try_from(user.get())?)
}

pub(super) async fn guild_choices(ctx: Context<'_>, query: &str) -> CreateAutocompleteResponse {
    let query = query.to_lowercase();
    // 名前の保管・参加/退出への追随はSerenityのキャッシュに任せる。
    let cache = &ctx.serenity_context().cache;
    let mut guilds: Vec<_> = cache
        .guilds()
        .into_iter()
        .filter(|id| ctx.guild_id().is_none_or(|current| current == *id))
        .filter(|id| {
            ctx.command().name != "add"
                || DomainGuildId::try_from(id.get())
                    .is_ok_and(|id| ctx.data().shop_channels.contains_key(&id))
        })
        .filter_map(|id| cache.guild(id).map(|guild| (id, guild.name.clone())))
        .filter(|(id, name)| {
            name.to_lowercase().contains(&query) || id.to_string().starts_with(&query)
        })
        .collect();
    guilds.sort_by_key(|(id, _)| *id);
    // ponytail: RESTを順に確認し2秒で打切り。大規模運用で不足したら並列数を制限して照会する。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut choices = Vec::new();
    for (id, name) in guilds {
        match timeout_at(deadline, authenticate(ctx.http(), id, ctx.author().id)).await {
            Ok(Ok(actor))
                if !matches!(ctx.command().name.as_ref(), "adjust" | "refund")
                    || actor.can_manage_not() =>
            {
                choices.push(AutocompleteChoice::new(
                    name.chars().take(100).collect::<String>(),
                    id.to_string(),
                ));
                if choices.len() == 25 {
                    break;
                }
            }
            Err(_) => break,
            _ => {}
        }
    }
    CreateAutocompleteResponse::new().set_choices(choices)
}

fn target_guild(current: Option<GuildId>, supplied: Option<&str>) -> Result<GuildId, &'static str> {
    let supplied = supplied
        .map(|value| {
            value
                .parse::<DomainGuildId>()
                .map(|id| GuildId::new(id.get()))
                .map_err(|_| "サーバーIDが不正です。")
        })
        .transpose()?;
    match (current, supplied) {
        (Some(current), Some(supplied)) if current != supplied => {
            Err("サーバー内ではそのサーバーを指定してください。")
        }
        (Some(current), _) => Ok(current),
        (None, Some(supplied)) => Ok(supplied),
        (None, None) => Err("DMでは guild に対象サーバーを指定してください。"),
    }
}

pub(super) fn parse_balance(value: &str) -> Result<Balance, &'static str> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("目標残高は0以上の整数で指定してください。");
    }
    value
        .parse::<i64>()
        .ok()
        .and_then(|value| Balance::try_from(value).ok())
        .ok_or("目標残高が上限を超えています。")
}

pub(super) async fn authenticate(
    http: &Http,
    guild_id: GuildId,
    user_id: UserId,
) -> Result<Actor, Error> {
    // キャッシュや入力フラグで認可しない。RESTでBotの参加と本人の所属・ロールを確認する。
    let guild = http.get_guild(guild_id).await?;
    let member = http.get_member(guild_id, user_id).await?;
    Ok(authenticated_actor(&guild, &member, user_id)?)
}

pub(super) fn authenticated_actor(
    guild: &PartialGuild,
    member: &Member,
    expected_user: UserId,
) -> Result<Actor, &'static str> {
    if member.user.id != expected_user || member.guild_id != guild.id {
        return Err("対象サーバーへの所属を確認できませんでした。");
    }
    let can_manage = guild.member_permissions(member).administrator();
    Ok(Actor::new(
        DomainGuildId::try_from(guild.id.get()).map_err(|_| "サーバーIDが不正です。")?,
        DomainUserId::try_from(expected_user.get()).map_err(|_| "ユーザーIDが不正です。")?,
        can_manage,
    ))
}

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
    let amount = parse_balance(price).map_err(|_| "価格は正のNOT整数で指定してください。")?;
    Ok(ItemDetails::try_new(
        title,
        description,
        Price::try_from(amount.get())?,
    )?)
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
        .await?
        .guild()
        .filter(|channel| {
            channel.guild_id.get() == actor.guild_id().get()
                && matches!(channel.kind, ChannelType::Text | ChannelType::News)
        })
        .ok_or("販売チャンネルは対象サーバーのテキストチャンネルを指定してください。")?;
    let guild = http.get_guild(channel.guild_id).await?;
    let bot = http.get_current_user().await?;
    let bot = http.get_member(channel.guild_id, bot.id).await?;
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

pub(super) async fn publication_status(
    http: &Http,
    data: &Data,
    actor: Actor,
    item_id: ItemId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let lock = data.publication_lock(item_id).await;
    let _publication = lock.lock().await;
    let result: Result<(MessageId, ChannelId), Error> = async {
        let mut session = data.database.begin_read().await?;
        let item = session.find_item(actor.guild_id(), item_id).await?;
        session.commit().await?;
        let item = item.ok_or(UseCaseError::ItemNotFound)?;
        if item.seller_id() != actor.user_id() {
            return Err(UseCaseError::PermissionDenied.into());
        }
        let (channel, guild_name) = selling_channel(http, data, actor).await?;
        let mut session = data.database.begin_read().await?;
        let existing = session.find_publication(actor.guild_id(), item_id).await?;
        session.commit().await?;
        let (embed, buttons) = {
            let actor = Actor::new(item.guild_id(), item.seller_id(), false);
            let embed = match item.target() {
                ItemTarget::Content(file) => item_embed(
                    item.details(),
                    actor,
                    file.filename(),
                    file.size_bytes(),
                    &guild_name,
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
        };
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
            return Ok((message, channel));
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
        Ok((message, channel))
    }
    .await;
    match result {
        Ok((message, channel)) => Ok((
            format!(
                "公開表示へ反映しました。商品ID: {item_id}\nhttps://discord.com/channels/{}/{}/{}",
                actor.guild_id(),
                channel,
                message
            ),
            vec![],
        )),
        Err(error)
            if matches!(
                error.downcast_ref::<UseCaseError>(),
                Some(UseCaseError::PermissionDenied | UseCaseError::ItemNotFound)
            ) =>
        {
            Err(error)
        }
        Err(_) => Ok((
            format!(
                "PublicationPending: 商品ID {item_id} は保存済みですが、公開を確認できません。再投稿してください。"
            ),
            vec![CreateActionRow::Buttons(vec![
                CreateButton::new(format!("content:republish:{}:{item_id}", actor.guild_id()))
                    .label("再投稿する")
                    .style(ButtonStyle::Primary),
            ])],
        )),
    }
}

pub(super) async fn listing_action(
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
            let actor = authenticate(http, GuildId::new(guild.get()), component.user.id).await?;
            publication_status(http, data, actor, item).await
        }
        ["content", "draft", id, action @ ("publish" | "cancel")] => {
            let draft = {
                let mut drafts = data.drafts.lock().await;
                let user = component.user.id.get();
                if !drafts
                    .get(&user)
                    .is_some_and(|draft| draft.id == *id && !draft.expired())
                {
                    return Err(
                        "下書きが失効しています。/content add から入力し直してください。".into(),
                    );
                }
                drafts.remove(&user).expect("checked draft")
            };
            if *action == "cancel" {
                return Ok(("出品をキャンセルしました。".into(), vec![]));
            }
            let result = async {
                let actor = authenticate(
                    http,
                    GuildId::new(draft.actor.guild_id().get()),
                    component.user.id,
                )
                .await?;
                if actor.user_id() != draft.actor.user_id()
                    || actor.guild_id() != draft.actor.guild_id()
                {
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

#[derive(Clone)]
pub(super) struct Upload {
    id: String,
    attachment: Attachment,
    guild: Option<DomainGuildId>,
    expires_at: Instant,
}

fn pending_upload<'a>(
    uploads: &'a HashMap<u64, Upload>,
    user: UserId,
    id: &str,
) -> Result<&'a Upload, Error> {
    uploads
        .get(&user.get())
        .filter(|upload| upload.id == id && Instant::now() < upload.expires_at)
        .ok_or_else(|| "出品入力が失効しています。DMへファイルを送り直してください。".into())
}

pub(super) async fn receive_upload(
    http: &Http,
    data: &Data,
    message: &Message,
) -> Result<(), Error> {
    if message.guild_id.is_some() || message.author.bot || message.attachments.is_empty() {
        return Ok(());
    }
    let valid = if message.attachments.len() == 1 {
        content::validate_attachment(&message.attachments[0])
    } else {
        Err("出品するファイルは1つずつ送ってください。".into())
    };
    if let Err(error) = valid {
        message
            .channel_id
            .send_message(
                http,
                CreateMessage::new()
                    .content(public_error_message(&error))
                    .allowed_mentions(CreateAllowedMentions::new()),
            )
            .await?;
        return Ok(());
    }
    let id = message.id.to_string();
    {
        let mut uploads = data.uploads.lock().await;
        uploads.retain(|_, upload| Instant::now() < upload.expires_at);
        uploads.insert(
            message.author.id.get(),
            Upload {
                id: id.clone(),
                attachment: message.attachments[0].clone(),
                guild: None,
                expires_at: Instant::now() + Duration::from_secs(300),
            },
        );
    }
    data.drafts.lock().await.remove(&message.author.id.get());
    let buttons = vec![
        CreateButton::new(format!("content:post:{id}:start"))
            .label("このファイルを出品する")
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("content:post:{id}:cancel")).label("キャンセル"),
    ];
    message.channel_id.send_message(http, CreateMessage::new()
        .content("ファイルを受け取りました。5分以内に出品内容を入力してください。最後の確認まで公開されません。")
        .components(vec![CreateActionRow::Buttons(buttons)]).allowed_mentions(CreateAllowedMentions::new())).await?;
    Ok(())
}

async fn selling_servers(
    http: &Http,
    data: &Data,
    user: UserId,
    id: &str,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    pending_upload(&*data.uploads.lock().await, user, id)?;
    let mut guilds: Vec<_> = data.shop_channels.keys().copied().collect();
    guilds.sort_by_key(|guild| guild.get());
    let last = guilds.len().saturating_sub(1) / 25;
    let page = page.min(last);
    let mut options = Vec::new();
    // shortcut: 設定済みサーバーを25件ずつ順に照会する。待ち時間が問題になったら並列数を制限して照会する。
    for id in guilds.into_iter().skip(page * 25).take(25) {
        let guild_id = GuildId::new(id.get());
        let Ok(guild) = http.get_guild(guild_id).await else {
            continue;
        };
        let Ok(member) = http.get_member(guild_id, user).await else {
            continue;
        };
        if authenticated_actor(&guild, &member, user).is_ok() {
            options.push(CreateSelectMenuOption::new(
                guild.name.chars().take(50).collect::<String>(),
                id.to_string(),
            ));
        }
    }
    let mut rows = Vec::new();
    if !options.is_empty() {
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(
                format!("content:post:{id}:server"),
                CreateSelectMenuKind::String { options },
            )
            .placeholder("出品先サーバー"),
        ));
    }
    let mut buttons = Vec::new();
    for (target, label) in [
        (page.saturating_sub(1), "前へ"),
        ((page + 1).min(last), "次へ"),
    ] {
        if target != page {
            buttons
                .push(CreateButton::new(format!("content:post:{id}:page:{target}")).label(label));
        }
    }
    buttons.push(CreateButton::new(format!("content:post:{id}:cancel")).label("キャンセル"));
    rows.push(CreateActionRow::Buttons(buttons));
    Ok((
        format!(
            "出品先を選択してください（{}/{}）。候補がなければ販売チャンネル設定と所属を確認してください。",
            page + 1,
            last + 1
        ),
        rows,
    ))
}

async fn listing_form(
    data: &Data,
    user: UserId,
    id: &str,
    selected: Option<&str>,
) -> Result<CreateModal, Error> {
    let mut uploads = data.uploads.lock().await;
    let upload = pending_upload(&uploads, user, id)?;
    let guild = selected
        .map(str::parse::<DomainGuildId>)
        .transpose()?
        .or(upload.guild)
        .filter(|guild| data.shop_channels.contains_key(guild))
        .ok_or("出品先が設定されていません。")?;
    uploads.get_mut(&user.get()).expect("checked upload").guild = Some(guild);
    Ok(
        CreateModal::new(format!("content:post:{id}:form:{guild}"), "出品内容の入力").components(
            vec![
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Short, "商品名", "title")
                        .min_length(1)
                        .max_length(256),
                ),
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Paragraph, "説明", "description")
                        .required(false)
                        .max_length(4096),
                ),
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Short, "価格（正のNOT整数）", "price")
                        .min_length(1)
                        .max_length(19),
                ),
            ],
        ),
    )
}

pub(super) async fn posting_component(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    if component.guild_id.is_none() {
        let hinted = if let ["content", "post", id, "start"] = parts.as_slice() {
            pending_upload(&*data.uploads.lock().await, component.user.id, id)
                .is_ok_and(|upload| upload.guild.is_some())
        } else {
            false
        };
        let modal = match parts.as_slice() {
            ["content", "post", id, "server"] => match &component.data.kind {
                ComponentInteractionDataKind::StringSelect { values } if values.len() == 1 => {
                    Some(listing_form(data, component.user.id, id, Some(&values[0])).await)
                }
                _ => Some(Err("出品先を1つ選んでください。".into())),
            },
            ["content", "post", id, "start"] if hinted => {
                Some(listing_form(data, component.user.id, id, None).await)
            }
            _ => None,
        };
        if let Some(modal) = modal {
            let response = match modal {
                Ok(modal) => CreateInteractionResponse::Modal(modal),
                Err(error) => CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(public_error_message(&error))
                        .ephemeral(true),
                ),
            };
            return Ok(component.create_response(http, response).await?);
        }
    }
    component
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let result = posting_action(http, data, component).await;
    let (text, buttons) = result.unwrap_or_else(|error| (public_error_message(&error), vec![]));
    component
        .edit_response(
            http,
            EditInteractionResponse::new()
                .content(text)
                .components(buttons)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
    Ok(())
}

async fn posting_action(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    if component.guild_id.is_some() {
        return Err("BotとのDMから操作してください。".into());
    }
    match parts.as_slice() {
        ["content", "post", id, "start"] => {
            selling_servers(http, data, component.user.id, id, 0).await
        }
        ["content", "post", id, "page", page] => {
            selling_servers(http, data, component.user.id, id, page.parse()?).await
        }
        ["content", "post", id, "cancel"] => {
            let mut uploads = data.uploads.lock().await;
            pending_upload(&uploads, component.user.id, id)?;
            uploads.remove(&component.user.id.get());
            Ok((
                "出品入力を取り消しました。ファイルは保存・公開していません。".into(),
                vec![],
            ))
        }
        _ => Err("出品入力が不正です。".into()),
    }
}

async fn listing_preview(
    http: &Http,
    data: &Data,
    modal: &ModalInteraction,
) -> Result<poise::CreateReply, Error> {
    if modal.guild_id.is_some() {
        return Err("BotとのDMから操作してください。".into());
    }
    let parts: Vec<_> = modal.data.custom_id.split(':').collect();
    let ["content", "post", id, "form", guild] = parts.as_slice() else {
        return Err("出品フォームが不正です。".into());
    };
    let guild: DomainGuildId = guild.parse()?;
    let upload = pending_upload(&*data.uploads.lock().await, modal.user.id, id)?.clone();
    if upload.guild != Some(guild) {
        return Err("出品先が変更されています。フォームを開き直してください。".into());
    }
    let mut fields = HashMap::new();
    for row in &modal.data.components {
        for field in &row.components {
            if let ActionRowComponent::InputText(input) = field
                && fields
                    .insert(
                        input.custom_id.as_str(),
                        input.value.as_deref().unwrap_or(""),
                    )
                    .is_some()
            {
                return Err("フォームの項目が重複しています。".into());
            }
        }
    }
    let title = fields.get("title").ok_or("商品名がありません。")?;
    let description = fields.get("description").copied().unwrap_or("");
    let price = fields.get("price").ok_or("価格がありません。")?;
    let actor = authenticate(http, GuildId::new(guild.get()), modal.user.id).await?;
    let (_, name) = selling_channel(http, data, actor).await?;
    let draft = ListingDraft::new(
        id.to_string(),
        actor,
        title.to_string(),
        description.into(),
        price,
        upload.attachment,
    )?;
    let mut uploads = data.uploads.lock().await;
    if pending_upload(&uploads, modal.user.id, id)?.guild != Some(guild) {
        return Err("出品先が変更されています。フォームを開き直してください。".into());
    }
    uploads.remove(&modal.user.id.get());
    Ok(remember_draft(data, draft, &name).await)
}

pub(super) async fn listing_modal(
    http: &Http,
    data: &Data,
    modal: &ModalInteraction,
) -> Result<(), Error> {
    modal
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let reply = listing_preview(http, data, modal)
        .await
        .unwrap_or_else(|error| {
            poise::CreateReply::default().content(public_error_message(&error))
        });
    modal
        .edit_response(
            http,
            reply
                .allowed_mentions(CreateAllowedMentions::new())
                .to_slash_initial_response_edit(EditInteractionResponse::new()),
        )
        .await?;
    Ok(())
}

fn resend_button(actor: Actor, purchase: LedgerEntryId) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("content:resend:{}:{purchase}", actor.guild_id()))
            .label(format!("無料再送 #{purchase}"))
            .style(ButtonStyle::Primary),
    ])
}

async fn prepare_purchase(
    http: &Http,
    data: &Data,
    actor: Actor,
    item: ItemId,
    id: String,
    now: SystemTime,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let quote = match data.shop.prepare_purchase(actor, item, &id, now).await {
        Err(UseCaseError::AlreadyPurchased) => {
            let purchase = data
                .shop
                .list_purchases(actor)
                .await?
                .into_iter()
                .find(|purchase| purchase.item().id() == item)
                .ok_or(UseCaseError::PurchaseNotFound)?;
            return Ok((
                "購入済みです。追加課金なしで再送できます。".into(),
                vec![resend_button(actor, purchase.buyer().id())],
            ));
        }
        result => result?,
    };
    if matches!(quote.item().target(), ItemTarget::Role(_)) {
        preflight_role(http, actor, quote.item()).await?;
    }
    let balance = data.not.get_balance(actor).await?;
    let price = quote.item().details().price().get();
    let after = balance.apply(NotAmount::new(-price))?;
    let guild = http.get_guild(GuildId::new(actor.guild_id().get())).await?;
    let message = CreateMessage::new()
        .embed(
            CreateEmbed::new()
                .title("購入の確認")
                .description(quote.item().details().name())
                .field("サーバー", guild.name, false)
                .field("出品者", format!("<@{}>", quote.item().seller_id()), false)
                .field(
                    "お届け内容",
                    match quote.item().target() {
                        ItemTarget::Content(file) => file.filename().to_owned(),
                        ItemTarget::Role(role) => format!("ロール <@&{role}>"),
                    },
                    false,
                )
                .field("価格", format!("{price} NOT"), true)
                .field("現在の残高", format!("{} NOT", balance.get()), true)
                .field("購入後の残高", format!("{} NOT", after.get()), true)
                .field("確認期限", "発行から5分", false),
        )
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("content:confirm:{id}:yes"))
                .label(format!("はい（{price} NOTで購入）"))
                .style(ButtonStyle::Success),
            CreateButton::new(format!("content:confirm:{id}:no"))
                .label("いいえ")
                .style(ButtonStyle::Secondary),
        ])])
        .allowed_mentions(CreateAllowedMentions::new());
    {
        let mut confirmations = data.confirmations.lock().await;
        confirmations.retain(|_, quote| now < quote.expires_at());
        confirmations.insert(id.clone(), quote);
    }
    if UserId::new(actor.user_id().get())
        .dm(http, message)
        .await
        .is_err()
    {
        data.confirmations.lock().await.remove(&id);
        return Err(
            "購入確認のDMを送信できませんでした。課金していません。DM設定を確認してください。"
                .into(),
        );
    }
    println!(
        "content confirmation guild={} item={} confirmation={id}",
        actor.guild_id(),
        item
    );
    Ok((
        "DMへ購入確認を送りました。表示された価格を確認してください。".into(),
        vec![],
    ))
}

async fn deliver_purchase(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    if let Ok(receipt) = data.shop.get_purchase(actor, purchase).await
        && matches!(receipt.item().target(), ItemTarget::Role(_))
    {
        return deliver_role(http, data, actor, purchase).await;
    }
    let result: Result<(), Error> = async {
        let file = data.shop.get_content_for_purchase(actor, purchase).await?;
        let bytes = content::read_content(&data.content_dir, &file).await?;
        let dm = UserId::new(actor.user_id().get())
            .create_dm_channel(http)
            .await?;
        data.shop.get_content_for_purchase(actor, purchase).await?;
        dm.send_message(
            http,
            CreateMessage::new()
                .content(format!(
                    "購入ID: {purchase}（再送を含め追加課金はありません）"
                ))
                .add_file(CreateAttachment::bytes(bytes, file.filename()))
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            println!(
                "content delivery guild={} purchase={purchase}",
                actor.guild_id()
            );
            Ok((
                format!("ファイルをDMへ送信しました。購入ID: {purchase}"),
                vec![],
            ))
        }
        Err(error)
            if matches!(
                error.downcast_ref::<UseCaseError>(),
                Some(
                    UseCaseError::PermissionDenied
                        | UseCaseError::PurchaseNotFound
                        | UseCaseError::AlreadyRefunded
                )
            ) =>
        {
            Err(error)
        }
        Err(_) => {
            eprintln!(
                "content delivery_pending guild={} purchase={purchase}",
                actor.guild_id()
            );
            Ok((
                format!(
                    "DeliveryPending: 購入ID {purchase} の配送を確認できません。無料再送または /content purchases から受け取れます。"
                ),
                vec![resend_button(actor, purchase)],
            ))
        }
    }
}

async fn confirm_purchase(
    http: &Http,
    data: &Data,
    user: UserId,
    id: &str,
    yes: bool,
    now: SystemTime,
    attachment_limit: u32,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let (guild, _) = id.split_once('-').ok_or("購入確認IDが不正です。")?;
    let guild: crate::domain::GuildId = guild.parse()?;
    let quote = {
        let mut confirmations = data.confirmations.lock().await;
        if confirmations.get(id).is_some_and(|quote| {
            quote.buyer_id().get() != user.get() || quote.item().guild_id() != guild
        }) {
            return Err(UseCaseError::PermissionDenied.into());
        }
        confirmations.remove(id)
    };
    if !yes && quote.is_some() {
        return Ok(("購入を取り消しました。課金していません。".into(), vec![]));
    }
    let actor = authenticate(http, GuildId::new(guild.get()), user).await?;
    if let Some(purchase) = data.shop.find_purchase_by_confirmation(actor, id).await? {
        return if yes {
            deliver_purchase(http, data, actor, purchase.buyer().id()).await
        } else {
            Ok((
                "決済済みです。この操作では返金されません。".into(),
                vec![resend_button(actor, purchase.buyer().id())],
            ))
        };
    }
    let quote = quote.filter(|quote| now < quote.expires_at())
        .ok_or("確認は処理中・取消済み・期限切れ、または再起動で失効しています。購入一覧を確認し、未購入なら商品から確認を作り直してください。")?;
    match quote.item().target() {
        ItemTarget::Content(file) => {
            if file.size_bytes() > u64::from(attachment_limit) {
                return Err("DMの添付上限を超えるため購入できません。課金していません。".into());
            }
            content::read_content(&data.content_dir, file).await?;
        }
        ItemTarget::Role(_) => preflight_role(http, actor, quote.item()).await?,
    }
    let purchase = match data.shop.purchase(actor, &quote, now).await {
        Err(UseCaseError::Database(_)) => {
            eprintln!(
                "content settlement_pending guild={} item={} confirmation={id}",
                actor.guild_id(),
                quote.item().id()
            );
            return Ok((
                format!(
                    "決済結果を確認できません。確認ID: {id}\n新しい購入操作をせず、同じ確認から結果を照合してください。"
                ),
                vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(format!("content:confirm:{id}:yes"))
                        .label("決済結果を確認する")
                        .style(ButtonStyle::Primary),
                ])],
            ));
        }
        result => result?,
    };
    println!(
        "content purchase guild={} item={} confirmation={id} purchase={}",
        actor.guild_id(),
        quote.item().id(),
        purchase.buyer().id()
    );
    deliver_purchase(http, data, actor, purchase.buyer().id()).await
}

pub(super) async fn list_purchases(
    http: &Http,
    data: &Data,
    actor: Actor,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let purchases = data.shop.list_purchases(actor).await?;
    if purchases.is_empty() {
        return Ok(("未返金の購入はありません。".into(), vec![]));
    }
    let last = (purchases.len() - 1) / 5;
    let page = page.min(last);
    let guild_name = guild_name(http, actor.guild_id()).await?;
    let mut text = format!(
        "サーバー: {guild_name}\n購入一覧（{}/{}）",
        page + 1,
        last + 1
    );
    let mut buttons = Vec::new();
    for purchase in purchases.iter().skip(page * 5).take(5) {
        text.push_str(&format!(
            "\n{} / 商品ID {} / 購入ID {} / 支払額 {} NOT",
            purchase
                .item()
                .details()
                .name()
                .chars()
                .take(80)
                .collect::<String>(),
            purchase.item().id(),
            purchase.buyer().id(),
            -purchase.buyer().amount().get()
        ));
        buttons.push(
            CreateButton::new(format!(
                "content:resend:{}:{}",
                actor.guild_id(),
                purchase.buyer().id()
            ))
            .label(format!(
                "{} #{}",
                if matches!(purchase.item().target(), ItemTarget::Role(_)) {
                    "ロール再照合"
                } else {
                    "無料再送"
                },
                purchase.buyer().id()
            ))
            .style(ButtonStyle::Primary),
        );
    }
    let mut rows = vec![CreateActionRow::Buttons(buttons)];
    let mut navigation = Vec::new();
    for (target, label) in [
        (page.saturating_sub(1), "前へ"),
        ((page + 1).min(last), "次へ"),
    ] {
        if target != page {
            navigation.push(
                CreateButton::new(format!("content:purchases:{}:{target}", actor.guild_id()))
                    .label(label),
            );
        }
    }
    if !navigation.is_empty() {
        rows.push(CreateActionRow::Buttons(navigation));
    }
    Ok((text, rows))
}

pub(super) async fn purchase_action(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
    now: SystemTime,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    match parts.as_slice() {
        ["content", "confirm", id, action @ ("yes" | "no")] if component.guild_id.is_none() => {
            confirm_purchase(
                http,
                data,
                component.user.id,
                id,
                *action == "yes",
                now,
                component.attachment_size_limit,
            )
            .await
        }
        [
            "content",
            kind @ ("buy" | "resend" | "purchases"),
            guild,
            target,
        ] => {
            let guild: crate::domain::GuildId = guild.parse()?;
            if component
                .guild_id
                .is_some_and(|current| current.get() != guild.get())
                || (*kind == "buy" && component.guild_id.is_none())
            {
                return Err("元の商品と同じサーバーから操作してください。".into());
            }
            let actor = authenticate(http, GuildId::new(guild.get()), component.user.id).await?;
            match *kind {
                "buy" => {
                    prepare_purchase(
                        http,
                        data,
                        actor,
                        target.parse()?,
                        format!("{guild}-{}", component.id),
                        now,
                    )
                    .await
                }
                "resend" => deliver_purchase(http, data, actor, target.parse()?).await,
                _ => list_purchases(http, data, actor, target.parse()?).await,
            }
        }
        _ => Err("購入操作が不正です。BotとのDMから確認してください。".into()),
    }
}

pub(super) async fn check_role(
    http: &Http,
    guild_id: GuildId,
    role_id: RoleId,
) -> Result<(), Error> {
    let guild = http.get_guild(guild_id).await?;
    if guild.id != guild_id {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let role = guild
        .roles
        .get(&role_id)
        .ok_or("販売対象のロールが見つかりません。")?;
    if role_id.get() == guild_id.get() || role.managed {
        return Err("@everyoneや外部サービスが管理するロールは販売できません。".into());
    }
    let bot = http.get_current_user().await?;
    let bot = role_member(http, guild_id, bot.id)
        .await?
        .ok_or("Botの所属を確認できません。")?;
    let highest = std::iter::once(RoleId::new(guild_id.get()))
        .chain(bot.roles.iter().copied())
        .filter_map(|id| guild.roles.get(&id))
        .max()
        .ok_or("Botのロールを確認できません。")?;
    if !guild
        .member_permissions(&bot)
        .contains(Permissions::MANAGE_ROLES)
        || role >= highest
    {
        return Err("Botに「ロールの管理」と、販売対象より上位のロールが必要です。".into());
    }
    Ok(())
}

async fn role_member(http: &Http, guild: GuildId, user: UserId) -> Result<Option<Member>, Error> {
    match http.get_member(guild, user).await {
        Ok(member) if member.user.id == user && member.guild_id == guild => Ok(Some(member)),
        Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(response)))
            if response.error.code == 10007 =>
        {
            Ok(None)
        }
        Ok(_) => Err(UseCaseError::PermissionDenied.into()),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn preflight_role(
    http: &Http,
    actor: Actor,
    item: &ShopItem,
) -> Result<(), Error> {
    if item.guild_id() != actor.guild_id() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let ItemTarget::Role(role) = item.target() else {
        return Err(UseCaseError::ItemNotFound.into());
    };
    let guild = GuildId::new(actor.guild_id().get());
    let role = RoleId::new(role.get());
    check_role(http, guild, role).await?;
    let buyer = role_member(http, guild, UserId::new(actor.user_id().get()))
        .await?
        .ok_or("購入者の所属を確認できません。")?;
    if buyer.roles.contains(&role) {
        return Err("このロールは既に保有しています。課金していません。".into());
    }
    Ok(())
}

fn role_pending(actor: Actor, purchase: LedgerEntryId) -> (String, Vec<CreateActionRow>) {
    eprintln!(
        "role delivery_pending guild={} purchase={purchase}",
        actor.guild_id()
    );
    (
        format!(
            "RoleDeliveryPending: 購入ID {purchase} のロール付与・返金を確認できません。追加購入せず、再照合または /role recover で復旧してください。"
        ),
        vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("content:resend:{}:{purchase}", actor.guild_id()))
                .label("ロールを再照合")
                .style(ButtonStyle::Primary),
        ])],
    )
}

pub(super) async fn deliver_role(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let receipt = match data.shop.get_purchase(actor, purchase).await {
        Err(UseCaseError::Database(_)) => return Ok(role_pending(actor, purchase)),
        result => result?,
    };
    if receipt.buyer().user_id() != actor.user_id() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let ItemTarget::Role(role) = receipt.item().target() else {
        return Err(UseCaseError::ItemNotFound.into());
    };
    let role = RoleId::new(role.get());
    let guild = GuildId::new(actor.guild_id().get());
    let user = UserId::new(actor.user_id().get());
    let lock = data.publication_lock(receipt.item().id()).await;
    let _delivery = lock.lock().await;
    match data.shop.get_purchase(actor, purchase).await {
        Err(UseCaseError::Database(_)) => return Ok(role_pending(actor, purchase)),
        result => {
            result?;
        }
    }
    let current = match role_member(http, guild, user).await {
        Ok(member) => member,
        Err(_) => return Ok(role_pending(actor, purchase)),
    };
    if current
        .as_ref()
        .is_some_and(|member| member.roles.contains(&role))
    {
        return Ok((
            format!("ロールを確認しました。購入ID: {purchase}（追加課金なし）"),
            vec![],
        ));
    }
    if current.is_some() && check_role(http, guild, role).await.is_ok() {
        data.shop.get_purchase(actor, purchase).await?;
        let _ = http
            .add_member_role(guild, user, role, Some("NotBot: purchased role"))
            .await;
    }
    match role_member(http, guild, user).await {
        Ok(Some(member)) if member.roles.contains(&role) => {
            println!(
                "role delivered guild={} purchase={purchase}",
                actor.guild_id()
            );
            Ok((
                format!("ロールを付与・確認しました。購入ID: {purchase}"),
                vec![],
            ))
        }
        Ok(_) => match data.shop.refund_role_delivery(actor, purchase).await {
            Ok(_) => Ok((
                format!(
                    "ロールを付与できなかったため、購入ID {purchase} の元の購入額を返金しました。"
                ),
                vec![],
            )),
            Err(_) => Ok(role_pending(actor, purchase)),
        },
        Err(_) => Ok(role_pending(actor, purchase)),
    }
}

pub(super) async fn reconcile_roles(http: &Http, data: &Data, guild: GuildId) -> Result<(), Error> {
    for purchase in data
        .shop
        .list_role_purchases(crate::domain::GuildId::try_from(guild.get())?)
        .await?
    {
        let actor = Actor::new(
            purchase.item().guild_id(),
            purchase.buyer().user_id(),
            false,
        );
        if deliver_role(http, data, actor, purchase.buyer().id())
            .await
            .is_err()
        {
            eprintln!(
                "role reconcile_failed guild={guild} purchase={}",
                purchase.buyer().id()
            );
        }
    }
    Ok(())
}

pub(super) async fn refund_role(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
    request: &str,
) -> Result<crate::domain::TradeResult, Error> {
    if !actor.can_manage_not() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let receipt = match data.shop.get_purchase(actor, purchase).await {
        Err(UseCaseError::AlreadyRefunded) => {
            return Ok(data.shop.refund(actor, purchase, request).await?);
        }
        other => other?,
    };
    let ItemTarget::Role(role) = receipt.item().target() else {
        return Ok(data.shop.refund(actor, purchase, request).await?);
    };
    let guild = GuildId::new(actor.guild_id().get());
    let user = UserId::new(receipt.buyer().user_id().get());
    let role = RoleId::new(role.get());
    let lock = data.publication_lock(receipt.item().id()).await;
    let _refund = lock.lock().await;
    match data.shop.get_purchase(actor, purchase).await {
        Err(UseCaseError::AlreadyRefunded) => {
            return Ok(data.shop.refund(actor, purchase, request).await?);
        }
        result => {
            result?;
        }
    }
    if role_member(http, guild, user)
        .await?
        .is_some_and(|member| member.roles.contains(&role))
    {
        let _ = http
            .remove_member_role(guild, user, role, Some("NotBot: refund role"))
            .await;
        if role_member(http, guild, user)
            .await?
            .is_some_and(|member| member.roles.contains(&role))
        {
            return Err("ロールの解除を確認できないため、返金していません。同じ購入IDで再試行してください。".into());
        }
    }
    match data.shop.refund(actor, purchase, request).await {
        Ok(result) => Ok(result),
        Err(error) => {
            // COMMIT結果不明なら、未返金の購入権限を再確認できた場合だけ戻す。
            if data.shop.get_purchase(actor, purchase).await.is_ok()
                && http
                    .add_member_role(guild, user, role, Some("NotBot: restore unrefunded role"))
                    .await
                    .is_err()
            {
                eprintln!("role restore_pending guild={guild} purchase={purchase}");
                return Err(format!("{error}\n未返金の購入権限は残っています。/role recover でロールを再照合してください。").into());
            }
            Err(error.into())
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/discord/services.rs"]
mod services_tests;

#[cfg(test)]
#[path = "../../../tests/discord/listings.rs"]
mod listings_tests;

#[cfg(test)]
#[path = "../../../tests/discord/posting.rs"]
mod posting_tests;

#[cfg(test)]
#[path = "../../../tests/discord/purchases.rs"]
mod purchases_tests;

#[cfg(test)]
#[path = "../../../tests/discord/roles.rs"]
mod roles_tests;
