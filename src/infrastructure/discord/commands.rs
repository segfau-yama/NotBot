use super::{Context, Data, events::public_error_message};
use crate::{
    domain::{
        Actor, GuildId as DomainGuildId, ItemDetails, ItemId, ItemTarget, LedgerEntryId,
        PurchaseResult, ShopItem,
    },
    infrastructure::discord::{
        not::NotRepository as DiscordNotRepository, shop::ShopRepository as DiscordShopRepository,
    },
    infrastructure::{Error, RepositoryError},
};
use serenity::all::*;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};
use tokio::{sync::Mutex, time::timeout_at};

/// サーバーごとのNOTを確認・管理します
#[poise::command(
    slash_command,
    subcommands("balance", "ranking", "adjust", "transfer"),
    install_context = "Guild",
    interaction_context = "Guild|BotDm"
)]
pub async fn not(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// 自分のNOT残高を確認
#[poise::command(slash_command, ephemeral)]
pub async fn balance(
    ctx: Context<'_>,
    #[description = "対象サーバー（DMでは必須。候補選択またはID入力）"]
    #[autocomplete = "Data::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    let guild_name = Data::guild_name(ctx.http(), actor.guild_id()).await?;
    let balance = ctx.data().sqlite_not.get_balance(actor).await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(
                CreateEmbed::new()
                    .title("NOT残高")
                    .colour(Colour::BLURPLE)
                    .field("サーバー", guild_name, false)
                    .field("ユーザー", format!("<@{}>", ctx.author().id), true)
                    .field("所持NOT", format!("**{} NOT**", balance.get()), true),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// NOT残高の上位10人を表示
#[poise::command(slash_command, ephemeral)]
pub async fn ranking(
    ctx: Context<'_>,
    #[description = "対象サーバー（DMでは必須。候補選択またはID入力）"]
    #[autocomplete = "Data::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    let guild_name = Data::guild_name(ctx.http(), actor.guild_id()).await?;
    let ranking = ctx.data().sqlite_not.get_ranking(actor, 10).await?;
    let mut embed = CreateEmbed::new()
        .title("NOTランキング（上位10人）")
        .colour(Colour::BLURPLE)
        .field("サーバー", guild_name, false);
    if ranking.is_empty() {
        embed = embed.description("取引履歴はまだありません。");
    }
    for (index, row) in ranking.iter().enumerate() {
        embed = embed.field(
            format!("{}位", index + 1),
            format!("<@{}> — **{} NOT**", row.user_id(), row.balance().get()),
            false,
        );
    }
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// 管理者が指定したNOTを加算・減算
#[poise::command(slash_command, ephemeral)]
pub async fn adjust(
    ctx: Context<'_>,
    #[description = "調整対象のサーバーメンバー"] user: User,
    #[description = "増減額（100で加算、-100で減算。符号付き64bit整数）"]
    #[min_length = 1]
    #[max_length = 20]
    amount: String,
    #[description = "対象サーバー（DMでは必須。候補選択またはID入力）"]
    #[autocomplete = "Data::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    let guild_name = Data::guild_name(ctx.http(), actor.guild_id()).await?;
    let target = DiscordNotRepository::new(ctx.http())
        .adjustment_target(actor, user.id)
        .await?;
    let amount = DiscordNotRepository::parse_amount(&amount)?;
    println!(
        "not adjustment_requested guild={} target={} amount={} request={}",
        actor.guild_id(),
        target,
        amount.get(),
        ctx.interaction.id
    );
    let entry = ctx
        .data()
        .sqlite_not
        .adjust_amount(actor, target, amount, &ctx.interaction.id.to_string())
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(
                CreateEmbed::new()
                    .title("NOT残高の調整")
                    .colour(Colour::BLURPLE)
                    .field("サーバー", guild_name, false)
                    .field("対象ユーザー", format!("<@{}>", user.id), true)
                    .field(
                        "増減額",
                        format!("**{:+} NOT**", entry.amount().get()),
                        true,
                    )
                    .footer(CreateEmbedFooter::new(format!("取引ID: {}", entry.id()))),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// 同じサーバーのメンバーへ100・500・1000 NOTを譲渡
#[poise::command(slash_command, ephemeral)]
pub async fn transfer(
    ctx: Context<'_>,
    #[description = "受取先のサーバーメンバー"] user: User,
    #[description = "譲渡するNOT"]
    #[choices(100, 500, 1000)]
    amount: i64,
    #[description = "対象サーバー（DMでは必須）"]
    #[autocomplete = "Data::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    let guild_name = Data::guild_name(ctx.http(), actor.guild_id()).await?;
    let target = DiscordNotRepository::new(ctx.http())
        .member_target(actor, user.id)
        .await?;
    println!(
        "not transfer_requested guild={} sender={} recipient={} amount={} request={}",
        actor.guild_id(),
        actor.user_id(),
        target,
        amount,
        ctx.interaction.id
    );
    let (sent, received) = ctx
        .data()
        .sqlite_not
        .transfer(actor, target, amount, &ctx.interaction.id.to_string())
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(
                CreateEmbed::new()
                    .title("NOTの譲渡")
                    .colour(Colour::BLURPLE)
                    .field("サーバー", guild_name, false)
                    .field("受取人", format!("<@{target}>"), true)
                    .field("譲渡額", format!("**{amount} NOT**"), true)
                    .footer(CreateEmbedFooter::new(format!(
                        "送信取引ID: {} / 受取取引ID: {}",
                        sent.id(),
                        received.id()
                    ))),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// コンテンツの売買
#[poise::command(
    slash_command,
    subcommands("add", "purchases", "update", "delete", "republish", "refund"),
    install_context = "Guild|User",
    interaction_context = "BotDm"
)]
pub async fn content(_: Context<'_>) -> Result<(), Error> {
    // Poiseの親コマンド。DB操作は各サブコマンドと出品・購入の確定時に行う。
    Ok(())
}

/// 内容を確認してから対象サーバーへ出品
#[poise::command(slash_command, dm_only)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "商品名"]
    #[min_length = 1]
    #[max_length = 256]
    title: String,
    #[description = "商品説明"]
    #[max_length = 4096]
    description: String,
    #[description = "価格（正のNOT整数）"]
    #[min_length = 1]
    #[max_length = 19]
    price: String,
    #[description = "出品先サーバー（候補選択またはID入力）"]
    #[autocomplete = "Data::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: String,
    #[description = "販売するファイル（1バイト〜20 MiB）"] file: Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if ctx.guild_id().is_some() {
        return Err("出品はBotとのDMから操作してください。".into());
    }
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    let (_, guild_name) = ctx.data().selling_channel(ctx.http(), actor).await?;
    let details = DiscordShopRepository::item_details(title, description, &price)?;
    // ここではプレビューを返す。DB保存は「出品する」を押したlisting_actionで行う。
    let draft = ListingDraft::new(ctx.interaction.id.to_string(), actor, details, file)?;
    let reply = {
        let mut uploads = ctx.data().uploads.lock().await;
        uploads.remove(&actor.user_id().get());
        ctx.data().remember_draft(draft, &guild_name).await
    };
    ctx.send(reply).await?;
    Ok(())
}

/// 購入したコンテンツの一覧と無料再送
#[poise::command(slash_command, dm_only)]
pub async fn purchases(
    ctx: Context<'_>,
    #[description = "対象サーバー（候補選択またはID入力）"]
    #[autocomplete = "Data::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    let purchases = ctx.data().sqlite_shop.list_purchases(actor).await?;
    let (text, buttons) = Data::purchase_page(ctx.http(), actor, &purchases, 0).await?;
    ctx.send(
        poise::CreateReply::default()
            .content(text)
            .components(buttons)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// 出品した商品の名前・説明・価格を変更（ファイルは変更不可）
#[poise::command(slash_command, dm_only)]
#[allow(clippy::too_many_arguments)]
pub async fn update(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "Data::guild_choices"]
    guild: String,
    #[description = "商品ID"] item: String,
    #[description = "公開表示にある現在の版"] revision: String,
    #[description = "変更後の商品名"]
    #[min_length = 1]
    #[max_length = 256]
    title: String,
    #[description = "変更後の商品説明"]
    #[max_length = 4096]
    description: String,
    #[description = "変更後の価格（正のNOT整数）"]
    #[min_length = 1]
    #[max_length = 19]
    price: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    let item = DiscordShopRepository::parse_item_id(&item)?;
    let revision = DiscordShopRepository::parse_revision(&revision)?;
    let details = DiscordShopRepository::item_details(title, description, &price)?;
    let data = ctx.data();
    let saved = {
        let lock = data.publication_lock(item).await;
        let _publication = lock.lock().await;
        data.sqlite_shop
            .update(actor, item, revision, details)
            .await?
    };
    println!(
        "content update guild={} item={} revision={}",
        actor.guild_id(),
        item,
        saved.revision()
    );
    let (status, buttons) = data.publication_status(ctx.http(), actor, item).await?;
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "商品ID {item} の版 {} を保存しました。\n{status}",
                saved.revision()
            ))
            .components(buttons),
    )
    .await?;
    Ok(())
}

/// 出品した商品の販売を終了（購入済みのファイルは再送可能）
#[poise::command(slash_command, dm_only)]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "Data::guild_choices"]
    guild: String,
    #[description = "商品ID"] item: String,
    #[description = "公開表示にある現在の版"] revision: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    let item = DiscordShopRepository::parse_item_id(&item)?;
    let revision = DiscordShopRepository::parse_revision(&revision)?;
    let data = ctx.data();
    let saved = {
        let lock = data.publication_lock(item).await;
        let _publication = lock.lock().await;
        data.sqlite_shop.delete(actor, item, revision).await?
    };
    println!(
        "content delete guild={} item={} revision={}",
        actor.guild_id(),
        item,
        saved.revision()
    );
    let (status, buttons) = data.publication_status(ctx.http(), actor, item).await?;
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "商品ID {item} は販売終了です（版 {}）。新規購入はできません。\n{status}",
                saved.revision()
            ))
            .components(buttons),
    )
    .await?;
    Ok(())
}

/// 保存済み商品の公開表示を最新状態へ同期、消えた投稿は作り直す
#[poise::command(slash_command, dm_only)]
pub async fn republish(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "Data::guild_choices"]
    guild: String,
    #[description = "商品ID"] item: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    let (text, buttons) = ctx
        .data()
        .publication_status(
            ctx.http(),
            actor,
            DiscordShopRepository::parse_item_id(&item)?,
        )
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .content(text)
            .components(buttons),
    )
    .await?;
    Ok(())
}

/// サーバー管理者が元の購入額を返金し、以後の無料再送を停止
#[poise::command(slash_command, dm_only)]
pub async fn refund(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "Data::guild_choices"]
    guild: String,
    #[description = "購入者の領収結果にある購入ID"] purchase: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), Some(&guild), ctx.author().id)
        .await?;
    ctx.say(
        ctx.data()
            .refund_purchase(
                ctx.http(),
                actor,
                DiscordShopRepository::parse_purchase_id(&purchase)?,
                &ctx.interaction.id.to_string(),
            )
            .await?,
    )
    .await?;
    Ok(())
}

/// サーバーのロール販売
#[poise::command(
    slash_command,
    subcommands("add_role", "recover"),
    install_context = "Guild",
    interaction_context = "Guild|BotDm"
)]
pub async fn role(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// サーバー管理者が既存ロールを商品として登録・公開
#[poise::command(slash_command, ephemeral, rename = "add")]
pub async fn add_role(
    ctx: Context<'_>,
    #[description = "販売する既存ロールのID"] role: String,
    #[description = "商品名"]
    #[min_length = 1]
    #[max_length = 256]
    title: String,
    #[description = "商品説明"]
    #[max_length = 4096]
    description: String,
    #[description = "価格（正のNOT整数）"]
    #[min_length = 1]
    #[max_length = 19]
    price: String,
    #[description = "対象サーバー（DMでは必須）"]
    #[autocomplete = "Data::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    if !actor.can_manage_not() {
        return Err(RepositoryError::PermissionDenied.into());
    }
    let role: crate::domain::RoleId = DiscordShopRepository::parse_role_id(&role)?;
    Data::check_role(
        ctx.http(),
        GuildId::new(actor.guild_id().get()),
        RoleId::new(role.get()),
    )
    .await?;
    ctx.data().selling_channel(ctx.http(), actor).await?;
    let item = ctx
        .data()
        .sqlite_shop
        .create_role(
            actor,
            role,
            DiscordShopRepository::item_details(title, description, &price)?,
        )
        .await?;
    let (text, buttons) = ctx
        .data()
        .publication_status(ctx.http(), actor, item.id())
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .content(text)
            .components(buttons),
    )
    .await?;
    Ok(())
}

/// 購入済みロールを照合・再付与（追加課金なし）
#[poise::command(slash_command, ephemeral)]
pub async fn recover(
    ctx: Context<'_>,
    #[description = "対象サーバー（DMでは必須）"]
    #[autocomplete = "Data::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = DiscordNotRepository::new(ctx.http())
        .actor(ctx.guild_id(), guild.as_deref(), ctx.author().id)
        .await?;
    let mut count = 0;
    for purchase in ctx.data().sqlite_shop.list_purchases(actor).await? {
        if matches!(purchase.item().target(), ItemTarget::Role(_)) {
            let (text, buttons) = ctx
                .data()
                .deliver_role(ctx.http(), actor, purchase.buyer().id())
                .await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(text)
                    .components(buttons),
            )
            .await?;
            count += 1;
        }
    }
    if count == 0 {
        ctx.say("未返金のロール購入はありません。").await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/discord/commands.rs"]
mod tests;

#[cfg(test)]
#[path = "../../../tests/discord/management.rs"]
mod management_tests;

impl Data {
    pub(super) async fn guild_name(http: &Http, guild: DomainGuildId) -> Result<String, Error> {
        Ok(http.get_guild(GuildId::new(guild.get())).await?.name)
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
                    || DiscordNotRepository::guild_id(*id)
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
            match timeout_at(
                deadline,
                DiscordNotRepository::new(ctx.http()).authenticate(id, ctx.author().id),
            )
            .await
            {
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
    async fn publication_lock(&self, item: ItemId) -> Arc<Mutex<()>> {
        let mut locks = self.publication_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&item).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(item, Arc::downgrade(&lock));
        lock
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
    pub(super) async fn remember_draft(
        &self,
        draft: ListingDraft,
        guild_name: &str,
    ) -> poise::CreateReply {
        let reply = draft.preview(guild_name);
        let mut drafts = self.drafts.lock().await;
        drafts.retain(|_, draft| !draft.expired());
        drafts.insert(draft.actor.user_id().get(), draft);
        reply
    }
    pub(super) async fn selling_channel(
        &self,
        http: &Http,
        actor: Actor,
    ) -> Result<(ChannelId, String), Error> {
        let channel_id = self
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
        if !permissions.contains(
            Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES | Permissions::EMBED_LINKS,
        ) {
            return Err(
                "Botに販売チャンネルの閲覧・メッセージ送信・埋め込みリンク権限が必要です。".into(),
            );
        }
        Ok((channel.id, guild.name))
    }
    pub(super) async fn publication_status(
        &self,
        http: &Http,
        actor: Actor,
        item_id: ItemId,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let lock = self.publication_lock(item_id).await;
        let _publication = lock.lock().await;
        let result: Result<(MessageId, ChannelId), Error> = async {
            let (item, existing) = self.sqlite_shop.get_publication(actor, item_id).await?;
            let (channel, guild_name) = self.selling_channel(http, actor).await?;
            let (embed, buttons) = {
                let embed = match item.target() {
                    ItemTarget::Content(file) => Self::item_embed(
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
            let (channel_id, message_id) =
                DiscordShopRepository::publication_ids(channel, message)?;
            self.sqlite_shop
                .record_publication(actor, &item, channel_id, message_id)
                .await?;
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
                    error.downcast_ref::<RepositoryError>(),
                    Some(RepositoryError::PermissionDenied | RepositoryError::ItemNotFound)
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
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        if component.guild_id.is_some() {
            return Err("BotとのDMから操作してください。".into());
        }
        let parts: Vec<_> = component.data.custom_id.split(':').collect();
        match parts.as_slice() {
            ["content", "republish", guild, item] => {
                let guild: crate::domain::GuildId = DiscordNotRepository::parse_guild_id(guild)?;
                let item = DiscordShopRepository::parse_item_id(item)?;
                let actor = DiscordNotRepository::new(http)
                    .authenticate(GuildId::new(guild.get()), component.user.id)
                    .await?;
                self.publication_status(http, actor, item).await
            }
            ["content", "draft", id, action @ ("publish" | "cancel")] => {
                let draft = {
                    let mut drafts = self.drafts.lock().await;
                    let user = component.user.id.get();
                    if !drafts
                        .get(&user)
                        .is_some_and(|draft| draft.id == *id && !draft.expired())
                    {
                        return Err(
                            "下書きが失効しています。/content add から入力し直してください。"
                                .into(),
                        );
                    }
                    drafts.remove(&user).expect("checked draft")
                };
                if *action == "cancel" {
                    return Ok(("出品をキャンセルしました。".into(), vec![]));
                }
                let result = async {
                    let actor = DiscordNotRepository::new(http)
                        .authenticate(
                            GuildId::new(draft.actor.guild_id().get()),
                            component.user.id,
                        )
                        .await?;
                    if actor.user_id() != draft.actor.user_id()
                        || actor.guild_id() != draft.actor.guild_id()
                    {
                        return Err(RepositoryError::PermissionDenied.into());
                    }
                    let bytes = DiscordShopRepository::download_attachment(
                        &self.download,
                        &draft.attachment,
                    )
                    .await?;
                    let content = self.discord_shop.clone();
                    let id = draft.id.clone();
                    let filename = draft.attachment.filename.clone();
                    let stored = tokio::task::spawn_blocking(move || {
                        content.store_content(actor, &id, &filename, bytes.as_slice())
                    })
                    .await
                    .map_err(|_| "ファイルの保存処理に失敗しました。")??;
                    Ok(self
                        .sqlite_shop
                        .create(actor, draft.details.clone(), stored)
                        .await?)
                }
                .await;
                match result {
                    Ok(item) => self.publication_status(http, draft.actor, item.id()).await,
                    Err(error) => {
                        if !draft.expired() {
                            self.drafts
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
    pub(super) async fn receive_upload(&self, http: &Http, message: &Message) -> Result<(), Error> {
        if message.guild_id.is_some() || message.author.bot || message.attachments.is_empty() {
            return Ok(());
        }
        let valid = if message.attachments.len() == 1 {
            DiscordShopRepository::validate_attachment(&message.attachments[0])
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
            let mut uploads = self.uploads.lock().await;
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
        self.drafts.lock().await.remove(&message.author.id.get());
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
        &self,
        http: &Http,
        user: UserId,
        id: &str,
        page: usize,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        pending_upload(&*self.uploads.lock().await, user, id)?;
        let mut guilds: Vec<_> = self.shop_channels.keys().copied().collect();
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
            if DiscordNotRepository::authenticated_actor(&guild, &member, user).is_ok() {
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
                buttons.push(
                    CreateButton::new(format!("content:post:{id}:page:{target}")).label(label),
                );
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
        &self,
        user: UserId,
        id: &str,
        selected: Option<&str>,
    ) -> Result<CreateModal, Error> {
        let mut uploads = self.uploads.lock().await;
        let upload = pending_upload(&uploads, user, id)?;
        let guild = selected
            .map(DiscordNotRepository::parse_guild_id)
            .transpose()?
            .or(upload.guild)
            .filter(|guild| self.shop_channels.contains_key(guild))
            .ok_or("出品先が設定されていません。")?;
        uploads.get_mut(&user.get()).expect("checked upload").guild = Some(guild);
        Ok(
            CreateModal::new(format!("content:post:{id}:form:{guild}"), "出品内容の入力")
                .components(vec![
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
                ]),
        )
    }
    pub(super) async fn posting_component(
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<(), Error> {
        let parts: Vec<_> = component.data.custom_id.split(':').collect();
        if component.guild_id.is_none() {
            let hinted = if let ["content", "post", id, "start"] = parts.as_slice() {
                pending_upload(&*self.uploads.lock().await, component.user.id, id)
                    .is_ok_and(|upload| upload.guild.is_some())
            } else {
                false
            };
            let modal = match parts.as_slice() {
                ["content", "post", id, "server"] => match &component.data.kind {
                    ComponentInteractionDataKind::StringSelect { values } if values.len() == 1 => {
                        Some(
                            self.listing_form(component.user.id, id, Some(&values[0]))
                                .await,
                        )
                    }
                    _ => Some(Err("出品先を1つ選んでください。".into())),
                },
                ["content", "post", id, "start"] if hinted => {
                    Some(self.listing_form(component.user.id, id, None).await)
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
        let result = self.posting_action(http, component).await;
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
        &self,
        http: &Http,
        component: &ComponentInteraction,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let parts: Vec<_> = component.data.custom_id.split(':').collect();
        if component.guild_id.is_some() {
            return Err("BotとのDMから操作してください。".into());
        }
        match parts.as_slice() {
            ["content", "post", id, "start"] => {
                self.selling_servers(http, component.user.id, id, 0).await
            }
            ["content", "post", id, "page", page] => {
                self.selling_servers(http, component.user.id, id, page.parse()?)
                    .await
            }
            ["content", "post", id, "cancel"] => {
                let mut uploads = self.uploads.lock().await;
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
        &self,
        http: &Http,
        modal: &ModalInteraction,
    ) -> Result<poise::CreateReply, Error> {
        if modal.guild_id.is_some() {
            return Err("BotとのDMから操作してください。".into());
        }
        let parts: Vec<_> = modal.data.custom_id.split(':').collect();
        let ["content", "post", id, "form", guild] = parts.as_slice() else {
            return Err("出品フォームが不正です。".into());
        };
        let guild: DomainGuildId = DiscordNotRepository::parse_guild_id(guild)?;
        let upload = pending_upload(&*self.uploads.lock().await, modal.user.id, id)?.clone();
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
        let actor = DiscordNotRepository::new(http)
            .authenticate(GuildId::new(guild.get()), modal.user.id)
            .await?;
        let (_, name) = self.selling_channel(http, actor).await?;
        let details =
            DiscordShopRepository::item_details(title.to_string(), description.into(), price)?;
        let draft = ListingDraft::new(id.to_string(), actor, details, upload.attachment)?;
        let mut uploads = self.uploads.lock().await;
        if pending_upload(&uploads, modal.user.id, id)?.guild != Some(guild) {
            return Err("出品先が変更されています。フォームを開き直してください。".into());
        }
        uploads.remove(&modal.user.id.get());
        Ok(self.remember_draft(draft, &name).await)
    }
    pub(super) async fn listing_modal(
        &self,
        http: &Http,
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
        let reply = self
            .listing_preview(http, modal)
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
    async fn prepare_purchase(
        &self,
        http: &Http,
        actor: Actor,
        item: ItemId,
        id: String,
        now: SystemTime,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let quote = match self
            .sqlite_shop
            .prepare_purchase(actor, item, &id, now)
            .await
        {
            Err(RepositoryError::AlreadyPurchased) => {
                let purchase = self
                    .sqlite_shop
                    .list_purchases(actor)
                    .await?
                    .into_iter()
                    .find(|purchase| purchase.item().id() == item)
                    .ok_or(RepositoryError::PurchaseNotFound)?;
                return Ok((
                    "購入済みです。追加課金なしで再送できます。".into(),
                    vec![Self::resend_button(actor, purchase.buyer().id())],
                ));
            }
            result => result?,
        };
        if matches!(quote.item().target(), ItemTarget::Role(_)) {
            Self::preflight_role(http, actor, quote.item()).await?;
        }
        let balance = self.sqlite_not.get_balance(actor).await?;
        let price = quote.item().details().price().get();
        let after =
            DiscordNotRepository::balance_after_purchase(balance, quote.item().details().price())?;
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
            let mut confirmations = self.confirmations.lock().await;
            confirmations.retain(|_, quote| now < quote.expires_at());
            confirmations.insert(id.clone(), quote);
        }
        if UserId::new(actor.user_id().get())
            .dm(http, message)
            .await
            .is_err()
        {
            self.confirmations.lock().await.remove(&id);
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
    async fn confirm_purchase(
        &self,
        http: &Http,
        user: UserId,
        id: &str,
        yes: bool,
        now: SystemTime,
        attachment_limit: u32,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let (guild, _) = id.split_once('-').ok_or("購入確認IDが不正です。")?;
        let guild: crate::domain::GuildId = DiscordNotRepository::parse_guild_id(guild)?;
        let quote = {
            let mut confirmations = self.confirmations.lock().await;
            if confirmations.get(id).is_some_and(|quote| {
                quote.buyer_id().get() != user.get() || quote.item().guild_id() != guild
            }) {
                return Err(RepositoryError::PermissionDenied.into());
            }
            confirmations.remove(id)
        };
        if !yes && quote.is_some() {
            return Ok(("購入を取り消しました。課金していません。".into(), vec![]));
        }
        let actor = DiscordNotRepository::new(http)
            .authenticate(GuildId::new(guild.get()), user)
            .await?;
        if let Some(purchase) = self
            .sqlite_shop
            .find_purchase_by_confirmation(actor, id)
            .await?
        {
            return if yes {
                self.deliver_purchase(http, actor, purchase.buyer().id())
                    .await
            } else {
                Ok((
                    "決済済みです。この操作では返金されません。".into(),
                    vec![Self::resend_button(actor, purchase.buyer().id())],
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
                self.discord_shop.read_content(file).await?;
            }
            ItemTarget::Role(_) => Self::preflight_role(http, actor, quote.item()).await?,
        }
        let purchase = match self.sqlite_shop.purchase(actor, &quote, now).await {
            Err(RepositoryError::Database(_)) => {
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
        self.deliver_purchase(http, actor, purchase.buyer().id())
            .await
    }
    pub(super) async fn purchase_action(
        &self,
        http: &Http,
        component: &ComponentInteraction,
        now: SystemTime,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let parts: Vec<_> = component.data.custom_id.split(':').collect();
        match parts.as_slice() {
            ["content", "confirm", id, action @ ("yes" | "no")] if component.guild_id.is_none() => {
                self.confirm_purchase(
                    http,
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
                let guild: crate::domain::GuildId = DiscordNotRepository::parse_guild_id(guild)?;
                if component
                    .guild_id
                    .is_some_and(|current| current.get() != guild.get())
                    || (*kind == "buy" && component.guild_id.is_none())
                {
                    return Err("元の商品と同じサーバーから操作してください。".into());
                }
                let actor = DiscordNotRepository::new(http)
                    .authenticate(GuildId::new(guild.get()), component.user.id)
                    .await?;
                match *kind {
                    "buy" => {
                        self.prepare_purchase(
                            http,
                            actor,
                            DiscordShopRepository::parse_item_id(target)?,
                            format!("{guild}-{}", component.id),
                            now,
                        )
                        .await
                    }
                    "resend" => {
                        self.deliver_purchase(
                            http,
                            actor,
                            DiscordShopRepository::parse_purchase_id(target)?,
                        )
                        .await
                    }
                    _ => {
                        let purchases = self.sqlite_shop.list_purchases(actor).await?;
                        Self::purchase_page(http, actor, &purchases, target.parse()?).await
                    }
                }
            }
            _ => Err("購入操作が不正です。BotとのDMから確認してください。".into()),
        }
    }
    pub(super) async fn purchase_page(
        http: &Http,
        actor: Actor,
        purchases: &[PurchaseResult],
        page: usize,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        if purchases.is_empty() {
            return Ok(("未返金の購入はありません。".into(), vec![]));
        }
        let last = (purchases.len() - 1) / 5;
        let page = page.min(last);
        let guild_name = Data::guild_name(http, actor.guild_id()).await?;
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
    fn resend_button(actor: Actor, purchase: LedgerEntryId) -> CreateActionRow {
        CreateActionRow::Buttons(vec![
            CreateButton::new(format!("content:resend:{}:{purchase}", actor.guild_id()))
                .label(format!("無料再送 #{purchase}"))
                .style(ButtonStyle::Primary),
        ])
    }
    async fn deliver_purchase(
        &self,
        http: &Http,
        actor: Actor,
        purchase: LedgerEntryId,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        if let Ok(receipt) = self.sqlite_shop.get_purchase(actor, purchase).await
            && matches!(receipt.item().target(), ItemTarget::Role(_))
        {
            return self.deliver_role(http, actor, purchase).await;
        }
        let result: Result<(), Error> = async {
            let file = self
                .sqlite_shop
                .get_content_for_purchase(actor, purchase)
                .await?;
            let bytes = self.discord_shop.read_content(&file).await?;
            let dm = UserId::new(actor.user_id().get())
                .create_dm_channel(http)
                .await?;
            self.sqlite_shop
                .get_content_for_purchase(actor, purchase)
                .await?;
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
                    error.downcast_ref::<RepositoryError>(),
                    Some(
                        RepositoryError::PermissionDenied
                            | RepositoryError::PurchaseNotFound
                            | RepositoryError::AlreadyRefunded
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
                    vec![Self::resend_button(actor, purchase)],
                ))
            }
        }
    }
    pub(super) async fn check_role(
        http: &Http,
        guild_id: GuildId,
        role_id: RoleId,
    ) -> Result<(), Error> {
        let guild = http.get_guild(guild_id).await?;
        if guild.id != guild_id {
            return Err(RepositoryError::PermissionDenied.into());
        }
        let role = guild
            .roles
            .get(&role_id)
            .ok_or("販売対象のロールが見つかりません。")?;
        if role_id.get() == guild_id.get() || role.managed {
            return Err("@everyoneや外部サービスが管理するロールは販売できません。".into());
        }
        let bot = http.get_current_user().await?;
        let bot = Self::role_member(http, guild_id, bot.id)
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
    async fn role_member(
        http: &Http,
        guild: GuildId,
        user: UserId,
    ) -> Result<Option<Member>, Error> {
        match http.get_member(guild, user).await {
            Ok(member) if member.user.id == user && member.guild_id == guild => Ok(Some(member)),
            Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
                response,
            ))) if response.error.code == 10007 => Ok(None),
            Ok(_) => Err(RepositoryError::PermissionDenied.into()),
            Err(error) => Err(error.into()),
        }
    }
    pub(super) async fn preflight_role(
        http: &Http,
        actor: Actor,
        item: &ShopItem,
    ) -> Result<(), Error> {
        if item.guild_id() != actor.guild_id() {
            return Err(RepositoryError::PermissionDenied.into());
        }
        let ItemTarget::Role(role) = item.target() else {
            return Err(RepositoryError::ItemNotFound.into());
        };
        let guild = GuildId::new(actor.guild_id().get());
        let role = RoleId::new(role.get());
        Self::check_role(http, guild, role).await?;
        let buyer = Self::role_member(http, guild, UserId::new(actor.user_id().get()))
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
        &self,
        http: &Http,
        actor: Actor,
        purchase: LedgerEntryId,
    ) -> Result<(String, Vec<CreateActionRow>), Error> {
        let receipt = match self.sqlite_shop.get_purchase(actor, purchase).await {
            Err(RepositoryError::Database(_)) => {
                return Ok(Self::role_pending(actor, purchase));
            }
            result => result?,
        };
        if receipt.buyer().user_id() != actor.user_id() {
            return Err(RepositoryError::PermissionDenied.into());
        }
        let ItemTarget::Role(role) = receipt.item().target() else {
            return Err(RepositoryError::ItemNotFound.into());
        };
        let role = RoleId::new(role.get());
        let guild = GuildId::new(actor.guild_id().get());
        let user = UserId::new(actor.user_id().get());
        let lock = self.publication_lock(receipt.item().id()).await;
        let _delivery = lock.lock().await;
        match self.sqlite_shop.get_purchase(actor, purchase).await {
            Err(RepositoryError::Database(_)) => {
                return Ok(Self::role_pending(actor, purchase));
            }
            result => {
                result?;
            }
        }
        let current = match Self::role_member(http, guild, user).await {
            Ok(member) => member,
            Err(_) => return Ok(Self::role_pending(actor, purchase)),
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
        if current.is_some() && Self::check_role(http, guild, role).await.is_ok() {
            self.sqlite_shop.get_purchase(actor, purchase).await?;
            let _ = http
                .add_member_role(guild, user, role, Some("NotBot: purchased role"))
                .await;
        }
        match Self::role_member(http, guild, user).await {
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
            Ok(_) => match self.sqlite_shop.refund_role_delivery(actor, purchase).await {
                Ok(_) => Ok((
                    format!(
                        "ロールを付与できなかったため、購入ID {purchase} の元の購入額を返金しました。"
                    ),
                    vec![],
                )),
                Err(_) => Ok(Self::role_pending(actor, purchase)),
            },
            Err(_) => Ok(Self::role_pending(actor, purchase)),
        }
    }
    pub(super) async fn reconcile_roles(&self, http: &Http, guild: GuildId) -> Result<(), Error> {
        for purchase in self
            .sqlite_shop
            .list_role_purchases(DiscordNotRepository::guild_id(guild)?)
            .await?
        {
            let actor = DiscordNotRepository::purchase_actor(&purchase);
            if self
                .deliver_role(http, actor, purchase.buyer().id())
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
        &self,
        http: &Http,
        actor: Actor,
        purchase: LedgerEntryId,
        request: &str,
    ) -> Result<crate::domain::RefundResult, Error> {
        if !actor.can_manage_not() {
            return Err(RepositoryError::PermissionDenied.into());
        }
        let receipt = match self.sqlite_shop.get_purchase(actor, purchase).await {
            Err(RepositoryError::AlreadyRefunded) => {
                return Ok(self.sqlite_shop.refund(actor, purchase, request).await?);
            }
            other => other?,
        };
        let ItemTarget::Role(role) = receipt.item().target() else {
            return Ok(self.sqlite_shop.refund(actor, purchase, request).await?);
        };
        let guild = GuildId::new(actor.guild_id().get());
        let user = UserId::new(receipt.buyer().user_id().get());
        let role = RoleId::new(role.get());
        let lock = self.publication_lock(receipt.item().id()).await;
        let _refund = lock.lock().await;
        match self.sqlite_shop.get_purchase(actor, purchase).await {
            Err(RepositoryError::AlreadyRefunded) => {
                return Ok(self.sqlite_shop.refund(actor, purchase, request).await?);
            }
            result => {
                result?;
            }
        }
        if Self::role_member(http, guild, user)
            .await?
            .is_some_and(|member| member.roles.contains(&role))
        {
            let _ = http
                .remove_member_role(guild, user, role, Some("NotBot: refund role"))
                .await;
            if Self::role_member(http, guild, user)
                .await?
                .is_some_and(|member| member.roles.contains(&role))
            {
                return Err("ロールの解除を確認できないため、返金していません。同じ購入IDで再試行してください。".into());
            }
        }
        match self.sqlite_shop.refund(actor, purchase, request).await {
            Ok(result) => Ok(result),
            Err(error) => {
                // COMMIT結果不明なら、未返金の購入権限を再確認できた場合だけ戻す。
                if self.sqlite_shop.get_purchase(actor, purchase).await.is_ok()
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
    pub(super) async fn refund_purchase(
        &self,
        http: &Http,
        actor: Actor,
        purchase: LedgerEntryId,
        request: &str,
    ) -> Result<String, Error> {
        let result = self.refund_role(http, actor, purchase, request).await?;
        println!(
            "content refund guild={} purchase={} request={}",
            actor.guild_id(),
            purchase,
            request
        );
        let sale = result
            .seller()
            .map(|entry| format!(" / 売上取消ID: {}", entry.id()))
            .unwrap_or_default();
        Ok(format!(
            "購入ID {purchase} の {} NOTを返金しました。返金取引ID: {}{sale}\n以後の受取・無料再送を停止します。",
            result.buyer().amount().get(),
            result.buyer().id()
        ))
    }
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
        details: ItemDetails,
        attachment: Attachment,
    ) -> Result<Self, Error> {
        DiscordShopRepository::validate_attachment(&attachment)?;
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
            .embed(Data::item_embed(&self.details, self.actor, &self.attachment.filename, self.attachment.size.into(), guild_name))
            .components(vec![CreateActionRow::Buttons(vec![
                CreateButton::new(format!("content:draft:{}:publish", self.id)).label("出品する").style(ButtonStyle::Success),
                CreateButton::new(format!("content:draft:{}:cancel", self.id)).label("キャンセル").style(ButtonStyle::Secondary),
            ])]).allowed_mentions(CreateAllowedMentions::new()).ephemeral(true)
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
