use crate::domain;
use std::time::{Duration, UNIX_EPOCH};

use serenity::json::{from_value, json};

use super::*;
use crate::infrastructure::discord::test_support::{
    self, MockDiscord, TestDirectory, actor, setup,
};

#[tokio::test]
async fn 投稿と反応の報酬は対象者を確認し再配信や再起動後も一度だけ付与する() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let now = UNIX_EPOCH + Duration::from_secs(100_000);
    let mut value = test_support::message();
    value["guild_id"] = json!("100");
    value["author"]["id"] = json!("201");
    value["author"]["bot"] = json!(false);
    let post: Message = from_value(value.clone()).unwrap();
    data.reward_message(&http, &post, now).await.unwrap();
    data.reward_message(&http, &post, now + Duration::from_secs(60))
        .await
        .unwrap();
    let buyer = domain::Actor::new(actor().guild_id(), domain::UserId::new(201).unwrap(), false);
    assert_eq!(data.sqlite_not.get_balance(buyer).await.unwrap().get(), 5);
    value["id"] = json!("901");
    value["type"] = json!(19);
    value["message_reference"] =
        json!({"message_id":"900","channel_id":"400","guild_id":"100","type":0});
    let reply: Message = from_value(value.clone()).unwrap();
    data.reward_message(&http, &reply, now + Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(data.sqlite_not.get_balance(buyer).await.unwrap().get(), 10);
    assert_eq!(
        data.sqlite_not.get_balance(actor()).await.unwrap().get(),
        20
    );
    for (id, field, input) in [
        (
            "902",
            "author",
            json!({"id":"42","username":"bot","bot":true}),
        ),
        ("903", "webhook_id", json!("42")),
        ("904", "guild_id", json!(null)),
        ("905", "author", json!({"id":"200","username":"self"})),
    ] {
        let mut ignored = value.clone();
        ignored["id"] = json!(id);
        ignored[field] = input;
        data.reward_message(
            &http,
            &from_value(ignored).unwrap(),
            now + Duration::from_secs(600),
        )
        .await
        .unwrap();
    }
    let mut click: Reaction = from_value(
        json!({"user_id":"201","channel_id":"400","message_id":"900","guild_id":"100",
        "emoji":{"name":"👍"},"burst":false,"type":0}),
    )
    .unwrap();
    data.reward_reaction(&http, &click, now).await.unwrap();
    assert_eq!(data.sqlite_not.get_balance(buyer).await.unwrap().get(), 11);
    assert_eq!(
        data.sqlite_not.get_balance(actor()).await.unwrap().get(),
        30
    );
    data.database.close().await.unwrap();
    let data = setup(&directory).await;
    click.emoji = ReactionType::Unicode("🎉".into());
    data.reward_reaction(&http, &click, now + Duration::from_secs(600))
        .await
        .unwrap();
    click.user_id = Some(UserId::new(200));
    data.reward_reaction(&http, &click, now + Duration::from_secs(600))
        .await
        .unwrap();
    click.user_id = Some(UserId::new(42));
    data.reward_reaction(&http, &click, now + Duration::from_secs(600))
        .await
        .unwrap();
    click.user_id = Some(UserId::new(202));
    mock.reward_message.lock().unwrap()["webhook_id"] = json!("42");
    data.reward_reaction(&http, &click, now + Duration::from_secs(600))
        .await
        .unwrap();
    mock.reward_message.lock().unwrap()["webhook_id"] = json!(null);
    mock.deny_member
        .store(true, std::sync::atomic::Ordering::Relaxed);
    data.reward_reaction(&http, &click, now + Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(data.sqlite_not.get_balance(buyer).await.unwrap().get(), 11);
    assert_eq!(
        data.sqlite_not.get_balance(actor()).await.unwrap().get(),
        30
    );
    assert!(mock.posts().is_empty());
    data.database.close().await.unwrap();
}
