use std::time::Duration;

use serenity::all::*;
use tokio::time::{Instant, timeout_at};

use super::Context;
use crate::domain::{Actor, Balance, GuildId as DomainGuildId, NotAmount, UserId as DomainUserId};
use crate::infrastructure::Error;

pub(super) async fn actor(ctx: Context<'_>, supplied: Option<&str>) -> Result<Actor, Error> {
    let guild_id = target_guild(ctx.guild_id(), supplied)?;
    Ok(authenticate(ctx.http(), guild_id, ctx.author().id).await?)
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
    let deadline = Instant::now() + Duration::from_secs(2);
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

pub(super) fn parse_amount(value: &str) -> Result<NotAmount, &'static str> {
    value
        .parse::<i64>()
        .map(NotAmount::new)
        .map_err(|_| "増減額は-9223372036854775808〜9223372036854775807の整数で指定してください。")
}

pub(super) async fn authenticate(
    http: &Http,
    guild_id: GuildId,
    user_id: UserId,
) -> Result<Actor, &'static str> {
    // キャッシュや入力フラグで認可しない。RESTでBotの参加と本人の所属・ロールを確認する。
    let guild = http
        .get_guild(guild_id)
        .await
        .map_err(|_| "Botの参加・サーバー情報を確認できませんでした。")?;
    let member = http
        .get_member(guild_id, user_id)
        .await
        .map_err(|_| "対象サーバーへの所属を確認できませんでした。")?;
    authenticated_actor(&guild, &member, user_id)
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

#[cfg(test)]
mod tests;
