use super::{Context, Data, services};
use crate::{
    domain::{Actor, ItemDetails, ItemId, ItemTarget, LedgerEntryId, NotAmount},
    infrastructure::{Error, UseCaseError},
};
use serenity::all::*;

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
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let guild_name = services::guild_name(ctx.http(), actor.guild_id()).await?;
    let balance = ctx.data().not.get_balance(actor).await?;
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
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let guild_name = services::guild_name(ctx.http(), actor.guild_id()).await?;
    let ranking = ctx.data().not.get_ranking(actor, 10).await?;
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
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let guild_name = services::guild_name(ctx.http(), actor.guild_id()).await?;
    let target = services::adjustment_target(ctx.http(), actor, user.id).await?;
    let amount = NotAmount::new(amount.parse().map_err(
        |_| "増減額は-9223372036854775808〜9223372036854775807の整数で指定してください。",
    )?);
    println!(
        "not adjustment_requested guild={} target={} amount={} request={}",
        actor.guild_id(),
        target,
        amount.get(),
        ctx.interaction.id
    );
    let entry = ctx
        .data()
        .not
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
    #[autocomplete = "services::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let guild_name = services::guild_name(ctx.http(), actor.guild_id()).await?;
    let target = services::member_target(ctx.http(), actor, user.id).await?;
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
        .not
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
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: String,
    #[description = "販売するファイル（1バイト〜20 MiB）"] file: Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if ctx.guild_id().is_some() {
        return Err("出品はBotとのDMから操作してください。".into());
    }
    let actor = services::actor(ctx, Some(&guild)).await?;
    let (_, guild_name) = services::selling_channel(ctx.http(), ctx.data(), actor).await?;
    let draft = services::ListingDraft::new(
        ctx.interaction.id.to_string(),
        actor,
        title,
        description,
        &price,
        file,
    )?;
    let reply = {
        let mut uploads = ctx.data().uploads.lock().await;
        uploads.remove(&actor.user_id().get());
        services::remember_draft(ctx.data(), draft, &guild_name).await
    };
    ctx.send(reply).await?;
    Ok(())
}

/// 購入したコンテンツの一覧と無料再送
#[poise::command(slash_command, dm_only)]
pub async fn purchases(
    ctx: Context<'_>,
    #[description = "対象サーバー（候補選択またはID入力）"]
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, Some(&guild)).await?;
    let (text, buttons) = services::list_purchases(ctx.http(), ctx.data(), actor, 0).await?;
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
    #[autocomplete = "services::guild_choices"]
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
    let actor = services::actor(ctx, Some(&guild)).await?;
    let (text, buttons) = update_item(
        ctx.http(),
        ctx.data(),
        actor,
        item.parse()?,
        services::parse_balance(&revision)?.get(),
        services::item_details(title, description, &price)?,
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

/// 出品した商品の販売を終了（購入済みのファイルは再送可能）
#[poise::command(slash_command, dm_only)]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "services::guild_choices"]
    guild: String,
    #[description = "商品ID"] item: String,
    #[description = "公開表示にある現在の版"] revision: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, Some(&guild)).await?;
    let (text, buttons) = delete_item(
        ctx.http(),
        ctx.data(),
        actor,
        item.parse()?,
        services::parse_balance(&revision)?.get(),
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

/// 保存済み商品の公開表示を最新状態へ同期、消えた投稿は作り直す
#[poise::command(slash_command, dm_only)]
pub async fn republish(
    ctx: Context<'_>,
    #[description = "対象サーバー"]
    #[autocomplete = "services::guild_choices"]
    guild: String,
    #[description = "商品ID"] item: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, Some(&guild)).await?;
    let (text, buttons) =
        services::publication_status(ctx.http(), ctx.data(), actor, item.parse()?).await?;
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
    #[autocomplete = "services::guild_choices"]
    guild: String,
    #[description = "購入者の領収結果にある購入ID"] purchase: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, Some(&guild)).await?;
    ctx.say(
        refund_purchase(
            ctx.http(),
            ctx.data(),
            actor,
            purchase.parse()?,
            &ctx.interaction.id.to_string(),
        )
        .await?,
    )
    .await?;
    Ok(())
}

pub(super) async fn update_item(
    http: &Http,
    data: &Data,
    actor: Actor,
    item: ItemId,
    revision: i64,
    details: ItemDetails,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let saved = {
        let lock = data.publication_lock(item).await;
        let _publication = lock.lock().await;
        data.shop.update(actor, item, revision, details).await?
    };
    println!(
        "content update guild={} item={} revision={}",
        actor.guild_id(),
        item,
        saved.revision()
    );
    let (status, buttons) = services::publication_status(http, data, actor, item).await?;
    Ok((
        format!(
            "商品ID {item} の版 {} を保存しました。\n{status}",
            saved.revision()
        ),
        buttons,
    ))
}

pub(super) async fn delete_item(
    http: &Http,
    data: &Data,
    actor: Actor,
    item: ItemId,
    revision: i64,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let saved = {
        let lock = data.publication_lock(item).await;
        let _publication = lock.lock().await;
        data.shop.delete(actor, item, revision).await?
    };
    println!(
        "content delete guild={} item={} revision={}",
        actor.guild_id(),
        item,
        saved.revision()
    );
    let (status, buttons) = services::publication_status(http, data, actor, item).await?;
    Ok((
        format!(
            "商品ID {item} は販売終了です（版 {}）。新規購入はできません。\n{status}",
            saved.revision()
        ),
        buttons,
    ))
}

pub(super) async fn refund_purchase(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
    request: &str,
) -> Result<String, Error> {
    let result = services::refund_role(http, data, actor, purchase, request).await?;
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
    #[autocomplete = "services::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    if !actor.can_manage_not() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let role: crate::domain::RoleId = role.parse()?;
    services::check_role(
        ctx.http(),
        GuildId::new(actor.guild_id().get()),
        RoleId::new(role.get()),
    )
    .await?;
    services::selling_channel(ctx.http(), ctx.data(), actor).await?;
    let item = ctx
        .data()
        .shop
        .create_role(
            actor,
            role,
            services::item_details(title, description, &price)?,
        )
        .await?;
    let (text, buttons) =
        services::publication_status(ctx.http(), ctx.data(), actor, item.id()).await?;
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
    #[autocomplete = "services::guild_choices"]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let mut count = 0;
    for purchase in ctx.data().shop.list_purchases(actor).await? {
        if matches!(purchase.item().target(), ItemTarget::Role(_)) {
            let (text, buttons) =
                services::deliver_role(ctx.http(), ctx.data(), actor, purchase.buyer().id())
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
