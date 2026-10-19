use serenity::all::{Attachment, User};

use super::{Context, services};
use crate::infrastructure::Error;

pub use super::management::{delete, refund, republish, update};

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
    let balance = ctx.data().not.get_balance(actor).await?;
    ctx.say(format!(
        "サーバー {}\nあなたの残高: {} NOT",
        actor.guild_id(),
        balance.get()
    ))
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
    let ranking = ctx.data().not.get_ranking(actor, 10).await?;
    let mut result = format!("サーバー {} のNOTランキング（上位10人）", actor.guild_id());
    if ranking.is_empty() {
        result.push_str("\n取引履歴はまだありません。");
    }
    for (index, row) in ranking.iter().enumerate() {
        result.push_str(&format!(
            "\n{}. <@{}>: {} NOT",
            index + 1,
            row.user_id(),
            row.balance().get()
        ));
    }
    ctx.say(result).await?;
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
    let target = services::adjustment_target(ctx.http(), actor, user.id).await?;
    let amount = services::parse_amount(&amount)?;
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
    ctx.say(format!(
        "サーバー {}\n<@{}> に {:+} NOTの調整を記録しました。取引ID: {}",
        actor.guild_id(),
        user.id,
        entry.amount().get(),
        entry.id()
    ))
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
    ctx.say(format!(
        "サーバー {}\n<@{}> に {} NOTを譲渡しました。送信取引ID: {} / 受取取引ID: {}",
        actor.guild_id(),
        target,
        amount,
        sent.id(),
        received.id()
    ))
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
    let (_, guild_name) = super::listings::selling_channel(ctx.http(), ctx.data(), actor).await?;
    let draft = super::listings::ListingDraft::new(
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
        super::listings::remember_draft(ctx.data(), draft, &guild_name).await
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
    let (text, buttons) = super::purchases::list(ctx.data(), actor, 0).await?;
    ctx.send(
        poise::CreateReply::default()
            .content(text)
            .components(buttons)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
