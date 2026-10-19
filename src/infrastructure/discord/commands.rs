use serenity::all::User;

use super::{Context, services};
use crate::infrastructure::Error;

/// サーバーごとのNOTを確認・管理します
#[poise::command(
    slash_command,
    subcommands("balance", "ranking", "adjust"),
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

/// 管理者が目標残高へ調整
#[poise::command(slash_command, ephemeral)]
pub async fn adjust(
    ctx: Context<'_>,
    #[description = "調整対象のサーバーメンバー"] user: User,
    #[description = "目標残高（0〜9223372036854775807の整数）"]
    #[min_length = 1]
    #[max_length = 19]
    balance: String,
    #[description = "対象サーバー（DMでは必須。候補選択またはID入力）"]
    #[autocomplete = "services::guild_choices"]
    #[min_length = 1]
    #[max_length = 20]
    guild: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let actor = services::actor(ctx, guild.as_deref()).await?;
    let target = services::adjustment_target(ctx.http(), actor, user.id).await?;
    let balance = services::parse_balance(&balance)?;
    let entry = ctx
        .data()
        .not
        .adjust_balance(actor, target, balance, &ctx.interaction.id.to_string())
        .await?;
    ctx.say(format!(
        "サーバー {}\n<@{}> の目標残高 {} NOTへの調整を受け付けました。\n記録した増減: {:+} NOT / 取引ID: {}",
        actor.guild_id(), user.id, balance.get(), entry.amount().get(), entry.id()
    )).await?;
    Ok(())
}