use super::{Data, services};
use crate::{
    domain::{Actor, LedgerReason},
    infrastructure::Error,
};
use serenity::all::*;
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

pub(super) async fn handle(ctx: &Context, event: &FullEvent, data: &Data) -> Result<(), Error> {
    let now = SystemTime::now();
    if let FullEvent::Ready { .. } = event {
        data.voice.lock().await.clear();
        println!("Discordに接続しました。/not・/content・/role が利用できます。");
    }
    if let FullEvent::GuildCreate { guild, .. } = event {
        seed_voice(data, guild.id, &guild.voice_states, Instant::now()).await;
        services::reconcile_roles(&ctx.http, data, guild.id).await?;
    }
    if let FullEvent::VoiceStateUpdate { new, .. } = event
        && let Some(guild) = new.guild_id
    {
        update_voice(data, guild, new, Instant::now()).await;
    }
    if let FullEvent::GuildDelete { incomplete, .. } = event {
        data.voice
            .lock()
            .await
            .retain(|(guild, _), _| *guild != incomplete.id);
    }
    if let FullEvent::ShardStageUpdate { event } = event
        && event.new != serenity::gateway::ConnectionStage::Connected
    {
        data.voice.lock().await.clear();
    }
    if let FullEvent::Resume { .. } = event {
        let guilds: Vec<_> = ctx
            .cache
            .guilds()
            .into_iter()
            .filter_map(|id| {
                ctx.cache
                    .guild(id)
                    .map(|guild| (id, guild.voice_states.clone()))
            })
            .collect();
        for (guild, states) in guilds {
            seed_voice(data, guild, &states, Instant::now()).await;
        }
    }
    if let FullEvent::Message { new_message } = event {
        reward_message(&ctx.http, data, new_message, now).await?;
        services::receive_upload(&ctx.http, data, new_message).await?;
    }
    if let FullEvent::ReactionAdd { add_reaction } = event {
        reward_reaction(&ctx.http, data, add_reaction, now).await?;
    }
    if let FullEvent::InteractionCreate {
        interaction: Interaction::Modal(modal),
    } = event
        && modal.data.custom_id.starts_with("content:post:")
    {
        services::listing_modal(&ctx.http, data, modal).await?;
    }
    if let FullEvent::InteractionCreate {
        interaction: Interaction::Component(component),
    } = event
    {
        handle_component(&ctx.http, data, component).await?;
    }
    Ok(())
}

pub(super) async fn handle_component(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(), Error> {
    let now = SystemTime::now();
    let kind = component
        .data
        .custom_id
        .strip_prefix("content:")
        .and_then(|id| id.split(':').next());
    let listing = matches!(kind, Some("draft" | "republish"));
    if kind == Some("post") {
        return services::posting_component(http, data, component).await;
    }
    if !listing && !matches!(kind, Some("buy" | "confirm" | "resend" | "purchases")) {
        return Ok(());
    }
    component
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let result = if listing {
        services::listing_action(http, data, component).await
    } else {
        services::purchase_action(http, data, component, now).await
    };
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

pub(super) async fn on_error(error: poise::FrameworkError<'_, std::sync::Arc<Data>, Error>) {
    if let Some(poise::Context::Application(ctx)) = error.ctx() {
        let content = match &error {
            poise::FrameworkError::Command { error, .. } => public_error_message(error),
            _ => "入力を確認して、もう一度操作してください。".into(),
        };
        if ctx
            .send(
                poise::CreateReply::default()
                    .content(content)
                    .ephemeral(true),
            )
            .await
            .is_err()
        {
            eprintln!(
                "Discord: 応答に失敗しました（操作ID: {}）",
                ctx.interaction.id
            );
        }
    } else {
        eprintln!("Discord: イベントの処理に失敗しました。");
    }
}

pub(super) fn public_error_message(error: &Error) -> String {
    // SDKエラーにはInteractionの秘密トークン付きURLが含まれ得る。
    if error.is::<serenity::Error>() {
        "Discordへの応答に失敗しました。".into()
    } else {
        error.to_string()
    }
}

pub(super) async fn participant(
    http: &Http,
    guild: GuildId,
    user: UserId,
) -> Result<Option<Actor>, Error> {
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

pub(super) async fn reward_message(
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

pub(super) async fn reward_reaction(
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

pub(super) struct Attendance {
    joined: Instant,
    checked: Instant,
    pending: Option<SystemTime>,
}

pub(super) async fn reward_voice(http: &Http, cache: &Cache, data: &Data) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        retain_connected(cache, data).await;
        tick_voice(http, data, Instant::now(), SystemTime::now()).await;
    }
}

async fn retain_connected(cache: &Cache, data: &Data) {
    data.voice.lock().await.retain(|(guild, user), _| {
        cache.guild(*guild).is_some_and(|guild| {
            guild
                .voice_states
                .get(user)
                .is_some_and(|state| state.channel_id.is_some())
        })
    });
}

pub(super) async fn update_voice(data: &Data, guild: GuildId, state: &VoiceState, now: Instant) {
    let mut voice = data.voice.lock().await;
    let key = (guild, state.user_id);
    if state.channel_id.is_none() || state.member.as_ref().is_some_and(|member| member.user.bot) {
        voice.remove(&key);
    } else {
        // チャンネル移動・ミュート変更では参加時間をリセットしない。
        voice.entry(key).or_insert(Attendance {
            joined: now,
            checked: now,
            pending: None,
        });
    }
}

pub(super) async fn seed_voice(
    data: &Data,
    guild: GuildId,
    states: &HashMap<UserId, VoiceState>,
    now: Instant,
) {
    data.voice.lock().await.retain(|(id, _), _| *id != guild);
    for state in states.values() {
        update_voice(data, guild, state, now).await;
    }
}

pub(super) async fn tick_voice(http: &Http, data: &Data, now: Instant, wall: SystemTime) {
    let due: Vec<_> = data
        .voice
        .lock()
        .await
        .iter()
        .filter(|(_, state)| {
            now.saturating_duration_since(state.checked) >= Duration::from_secs(60)
        })
        .map(|(&(guild, user), state)| (guild, user, state.joined))
        .collect();
    for (guild, user, joined) in due {
        let actor = participant(http, guild, user).await;
        let mut voice = data.voice.lock().await;
        let Some(state) = voice.get_mut(&(guild, user)).filter(|state| {
            state.joined == joined
                && now.saturating_duration_since(state.checked) >= Duration::from_secs(60)
        }) else {
            continue;
        };
        let actor = match actor {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                voice.remove(&(guild, user));
                continue;
            }
            Err(_) => {
                state.checked = now;
                eprintln!("voice membership_failed guild={guild} user={user}");
                continue;
            }
        };
        let earned_at = *state.pending.get_or_insert(wall);
        state.checked = now;
        // DB確定の間だけ入退室との順序を揃える。RESTはロック外で行う。
        match data.not.grant_voice_reward(actor, earned_at).await {
            Ok(_) => {
                state.pending = None;
            }
            Err(_) => {
                // 結果不明でも同じ獲得時刻のキーで照合する。
                eprintln!("voice reward_failed guild={guild} user={user}");
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/discord/rewards.rs"]
mod rewards_tests;

#[cfg(test)]
#[path = "../../../tests/discord/voice.rs"]
mod voice_tests;
