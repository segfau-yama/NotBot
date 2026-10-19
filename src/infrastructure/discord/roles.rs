use serenity::all::*;

use super::{Context, Data, listings, services};
use crate::{
    domain::{Actor, ItemTarget, LedgerEntryId, ShopItem},
    infrastructure::{Error, UseCaseError},
};

/// サーバーのロール販売
#[poise::command(
    slash_command,
    subcommands("add", "recover"),
    install_context = "Guild",
    interaction_context = "Guild|BotDm"
)]
pub async fn role(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// サーバー管理者が既存ロールを商品として登録・公開
#[poise::command(slash_command, ephemeral)]
pub async fn add(
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
    check_role(
        ctx.http(),
        GuildId::new(actor.guild_id().get()),
        RoleId::new(role.get()),
    )
    .await?;
    listings::selling_channel(ctx.http(), ctx.data(), actor).await?;
    let item = ctx
        .data()
        .shop
        .create_role(
            actor,
            role,
            listings::item_details(title, description, &price)?,
        )
        .await?;
    let (text, buttons) =
        listings::publication_status(ctx.http(), ctx.data(), actor, item.id()).await?;
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
                deliver(ctx.http(), ctx.data(), actor, purchase.buyer().id()).await?;
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
    let bot = member(http, guild_id, bot.id)
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

async fn member(http: &Http, guild: GuildId, user: UserId) -> Result<Option<Member>, Error> {
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

pub(super) async fn preflight(http: &Http, actor: Actor, item: &ShopItem) -> Result<(), Error> {
    if item.guild_id() != actor.guild_id() {
        return Err(UseCaseError::PermissionDenied.into());
    }
    let ItemTarget::Role(role) = item.target() else {
        return Err(UseCaseError::ItemNotFound.into());
    };
    let guild = GuildId::new(actor.guild_id().get());
    let role = RoleId::new(role.get());
    check_role(http, guild, role).await?;
    let buyer = member(http, guild, UserId::new(actor.user_id().get()))
        .await?
        .ok_or("購入者の所属を確認できません。")?;
    if buyer.roles.contains(&role) {
        return Err("このロールは既に保有しています。課金していません。".into());
    }
    Ok(())
}

fn pending(actor: Actor, purchase: LedgerEntryId) -> (String, Vec<CreateActionRow>) {
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

pub(super) async fn deliver(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let receipt = match data.shop.get_purchase(actor, purchase).await {
        Err(UseCaseError::Database(_)) => return Ok(pending(actor, purchase)),
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
        Err(UseCaseError::Database(_)) => return Ok(pending(actor, purchase)),
        result => {
            result?;
        }
    }
    let current = match member(http, guild, user).await {
        Ok(member) => member,
        Err(_) => return Ok(pending(actor, purchase)),
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
    match member(http, guild, user).await {
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
            Err(_) => Ok(pending(actor, purchase)),
        },
        Err(_) => Ok(pending(actor, purchase)),
    }
}

pub(super) async fn reconcile(http: &Http, data: &Data, guild: GuildId) -> Result<(), Error> {
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
        if deliver(http, data, actor, purchase.buyer().id())
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

pub(super) async fn refund(
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
    if member(http, guild, user)
        .await?
        .is_some_and(|member| member.roles.contains(&role))
    {
        let _ = http
            .remove_member_role(guild, user, role, Some("NotBot: refund role"))
            .await;
        if member(http, guild, user)
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
mod tests;
