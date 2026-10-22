use std::time::UNIX_EPOCH;

use sea_orm::ConnectionTrait;
use serenity::json::{from_value, json, to_value};

use super::*;
use crate::infrastructure::discord::test_support::*;

fn state(user: u64) -> VoiceState {
    from_value(
        json!({"guild_id":"100", "user_id":user.to_string(), "channel_id":"500",
        "session_id":"offline-voice-session", "deaf":false, "mute":false,
        "self_deaf":false, "self_mute":false, "self_video":false, "suppress":false}),
    )
    .unwrap()
}

#[tokio::test]
async fn 通話報酬は参加中の一分ごとに付与し移動では継続して退出や停止中は付与しない() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let guild = GuildId::new(100);
    let joined = Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(100_000);
    let mut member = state(200);
    data.update_voice(guild, &member, joined).await;
    data.tick_voice(
        &http,
        joined + Duration::from_millis(59_999),
        wall + Duration::from_millis(59_999),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 0);
    assert!(mock.requests.lock().unwrap().is_empty());
    data.tick_voice(
        &http,
        joined + Duration::from_secs(60),
        wall + Duration::from_secs(60),
    )
    .await;
    data.tick_voice(
        &http,
        joined + Duration::from_secs(60),
        wall + Duration::from_secs(60),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 1);
    member.channel_id = Some(ChannelId::new(501));
    member.self_mute = true;
    data.update_voice(guild, &member, joined + Duration::from_secs(61))
        .await;
    data.tick_voice(
        &http,
        joined + Duration::from_millis(119_999),
        wall + Duration::from_millis(119_999),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 1);
    data.tick_voice(
        &http,
        joined + Duration::from_secs(120),
        wall + Duration::from_secs(120),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 2);
    data.update_voice(guild, &state(42), joined + Duration::from_secs(120))
        .await;
    data.tick_voice(
        &http,
        joined + Duration::from_secs(180),
        wall + Duration::from_secs(180),
    )
    .await;
    assert!(
        !data
            .voice
            .lock()
            .await
            .contains_key(&(guild, UserId::new(42)))
    );
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 3);
    member.channel_id = None;
    data.update_voice(guild, &member, joined + Duration::from_secs(181))
        .await;
    data.tick_voice(
        &http,
        joined + Duration::from_secs(240),
        wall + Duration::from_secs(240),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 3);
    member.channel_id = Some(ChannelId::new(500));
    data.update_voice(guild, &member, joined + Duration::from_secs(241))
        .await;
    data.tick_voice(
        &http,
        joined + Duration::from_secs(300),
        wall + Duration::from_secs(300),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 3);
    data.tick_voice(
        &http,
        joined + Duration::from_secs(301),
        wall + Duration::from_secs(301),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 4);
    let connection = raw(&directory).await;
    connection.execute_unprepared("CREATE TRIGGER reject_voice BEFORE INSERT ON not_transaction WHEN NEW.event_key LIKE 'voice:%' BEGIN SELECT RAISE(ABORT, 'injected'); END;").await.unwrap();
    data.tick_voice(
        &http,
        joined + Duration::from_secs(361),
        wall + Duration::from_secs(361),
    )
    .await;
    let pending = data
        .voice
        .lock()
        .await
        .get(&(guild, UserId::new(200)))
        .unwrap()
        .pending
        .unwrap();
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 4);
    connection
        .execute_unprepared("DROP TRIGGER reject_voice")
        .await
        .unwrap();
    // DBでは確定し、呼出し側だけ結果を失った状態から、翌分も同じキーで復元する。
    data.sqlite_not
        .grant_voice_reward(actor(), pending)
        .await
        .unwrap()
        .unwrap();
    data.tick_voice(
        &http,
        joined + Duration::from_secs(421),
        wall + Duration::from_secs(421),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 5);
    assert!(
        data.voice
            .lock()
            .await
            .get(&(guild, UserId::new(200)))
            .unwrap()
            .pending
            .is_none()
    );
    connection.close().await.unwrap();
    data.database.close().await.unwrap();
    let data = setup(&directory).await;
    let restart = joined + Duration::from_secs(3600);
    let states = HashMap::from([(member.user_id, member)]);
    data.seed_voice(guild, &states, restart).await;
    data.tick_voice(
        &http,
        restart + Duration::from_secs(59),
        wall + Duration::from_secs(3659),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 5);
    data.tick_voice(
        &http,
        restart + Duration::from_secs(60),
        wall + Duration::from_secs(3660),
    )
    .await;
    assert_eq!(data.sqlite_not.get_balance(actor()).await.unwrap().get(), 6);
    let cache = Cache::default();
    let mut cached_guild = Guild::default();
    cached_guild.id = guild;
    cached_guild.voice_states = states;
    let mut created: GuildCreateEvent = from_value(to_value(cached_guild).unwrap()).unwrap();
    cache.update(&mut created);
    data.retain_connected(&cache).await;
    assert!(
        data.voice
            .lock()
            .await
            .contains_key(&(guild, UserId::new(200)))
    );
    let mut left = state(200);
    left.channel_id = None;
    let mut left: VoiceStateUpdateEvent = from_value(to_value(left).unwrap()).unwrap();
    cache.update(&mut left);
    // 入退室ハンドラが遅れても、SDKの最新状態で退出済みを除外する。
    data.retain_connected(&cache).await;
    assert!(data.voice.lock().await.is_empty());
    assert!(mock.posts().is_empty());
    data.database.close().await.unwrap();
}
