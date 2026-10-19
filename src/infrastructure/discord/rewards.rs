use std::time::SystemTime;

use serenity::all::*;

use super::Data;
use crate::{
    domain::{Actor, LedgerReason},
    infrastructure::Error,
};

async fn participant(http: &Http, guild: GuildId, user: UserId) -> Result<Option<Actor>, Error> {
    let member =
        match http.get_member(guild, user).await {
            Ok(member) => member,
            Err(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
                response,
            ))) if response.error.code == 10007 => return Ok(None),
            Err(error) => return Err(error.into()),
        };
    if member.user.id != user || member.guild_id != guild || member.user.bot {
        return Ok(None);
    }
    Ok(Some(Actor::new(
        crate::domain::GuildId::try_from(guild.get())?,
        crate::domain::UserId::try_from(user.get())?,
        false,
    )))
}

fn ordinary(message: &Message) -> bool {
    !message.author.bot
        && message.webhook_id.is_none()
        && matches!(
            message.kind,
            MessageType::Regular | MessageType::InlineReply
        )
}

pub(super) async fn message(
    http: &Http,
    data: &Data,
    message: &Message,
    now: SystemTime,
) -> Result<(), Error> {
    let Some(guild) = message.guild_id else {
        return Ok(());
    };
    if !ordinary(message) {
        return Ok(());
    }
    let original = if message.kind == MessageType::InlineReply {
        let Some(reference) = message.message_reference.as_ref() else {
            return Ok(());
        };
        let Some(id) = reference.message_id else {
            return Ok(());
        };
        if reference.kind != MessageReferenceKind::Default
            || reference.channel_id != message.channel_id
            || reference.guild_id.is_some_and(|id| id != guild)
        {
            return Ok(());
        }
        let original = match message.referenced_message.as_deref() {
            Some(message) => message.clone(),
            None => http.get_message(message.channel_id, id).await?,
        };
        if original.id != id
            || original.channel_id != message.channel_id
            || original.guild_id.is_some_and(|id| id != guild)
            || !ordinary(&original)
            || original.author.id == message.author.id
        {
            return Ok(());
        }
        Some(original)
    } else {
        None
    };
    let Some(actor) = participant(http, guild, message.author.id).await? else {
        return Ok(());
    };
    let request = message.id.to_string();
    data.not
        .grant_reward(actor, LedgerReason::Message, &request, now)
        .await?;
    if let Some(original) = original
        && let Some(actor) = participant(http, guild, original.author.id).await?
    {
        data.not
            .grant_reward(actor, LedgerReason::ReplyReceived, &request, now)
            .await?;
    }
    Ok(())
}

pub(super) async fn reaction(
    http: &Http,
    data: &Data,
    reaction: &Reaction,
    now: SystemTime,
) -> Result<(), Error> {
    let (Some(guild), Some(user)) = (reaction.guild_id, reaction.user_id) else {
        return Ok(());
    };
    if reaction
        .member
        .as_ref()
        .is_some_and(|member| member.user.bot)
    {
        return Ok(());
    }
    let original = http
        .get_message(reaction.channel_id, reaction.message_id)
        .await?;
    if original.id != reaction.message_id
        || original.channel_id != reaction.channel_id
        || original.guild_id.is_some_and(|id| id != guild)
        || !ordinary(&original)
        || original.author.id == user
    {
        return Ok(());
    }
    let Some(actor) = participant(http, guild, user).await? else {
        return Ok(());
    };
    let Some(author) = participant(http, guild, original.author.id).await? else {
        return Ok(());
    };
    let request = format!("{}-{user}", reaction.message_id);
    data.not
        .grant_reward(actor, LedgerReason::Reaction, &request, now)
        .await?;
    data.not
        .grant_reward(author, LedgerReason::ReactionReceived, &request, now)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
