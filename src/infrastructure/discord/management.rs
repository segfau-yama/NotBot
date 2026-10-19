use serenity::all::{CreateActionRow, Http};

use super::{Context, Data, listings, services};
use crate::{
    domain::{Actor, ItemDetails, ItemId, LedgerEntryId},
    infrastructure::Error,
};

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
        listings::item_details(title, description, &price)?,
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
        listings::publication_status(ctx.http(), ctx.data(), actor, item.parse()?).await?;
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
    let (status, buttons) = listings::publication_status(http, data, actor, item).await?;
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
    let (status, buttons) = listings::publication_status(http, data, actor, item).await?;
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
    let result = super::roles::refund(http, data, actor, purchase, request).await?;
    println!(
        "content refund guild={} purchase={} request={}",
        actor.guild_id(),
        purchase,
        request
    );
    Ok(format!(
        "購入ID {purchase} の {} NOTを返金しました。返金取引ID: {} / 売上取消ID: {}\n以後の受取・無料再送を停止します。",
        result.buyer().amount().get(),
        result.buyer().id(),
        result.seller().id()
    ))
}

#[cfg(test)]
mod tests;
