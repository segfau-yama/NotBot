use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

use serenity::all::*;

use super::{Data, events::error_summary, rewards};

pub(super) struct Attendance {
    joined: Instant,
    checked: Instant,
    pending: Option<SystemTime>,
}

pub(super) async fn run(http: &Http, cache: &Cache, data: &Data) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        retain_connected(cache, data).await;
        tick(http, data, Instant::now(), SystemTime::now()).await;
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

pub(super) async fn update(data: &Data, guild: GuildId, state: &VoiceState, now: Instant) {
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

pub(super) async fn seed(
    data: &Data,
    guild: GuildId,
    states: &HashMap<UserId, VoiceState>,
    now: Instant,
) {
    data.voice.lock().await.retain(|(id, _), _| *id != guild);
    for state in states.values() {
        update(data, guild, state, now).await;
    }
}

pub(super) async fn tick(http: &Http, data: &Data, now: Instant, wall: SystemTime) {
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
        let actor = rewards::participant(http, guild, user).await;
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
            Err(error) => {
                state.checked = now;
                eprintln!(
                    "voice membership_failed guild={guild} user={user} {}",
                    error_summary(&error)
                );
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
            Err(error) => {
                // 結果不明でも同じ獲得時刻のキーで照合する。
                eprintln!(
                    "voice reward_failed guild={guild} user={user} {}",
                    error_summary(&error.into())
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
