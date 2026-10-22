use crate::domain;
use std::sync::atomic::Ordering;

use serenity::{
    all::{GuildId, UserId},
    json::{json, to_value},
};

use crate::infrastructure::discord::test_support::*;

#[tokio::test]
async fn 残高調整は管理者だけが行え所属するメンバー間では定額を譲渡できる() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let sender = data
        .discord_not
        .authenticate(&http, GuildId::new(100), UserId::new(200))
        .await
        .unwrap();
    let admin = data
        .discord_not
        .authenticate(&http, GuildId::new(100), UserId::new(999))
        .await
        .unwrap();
    assert!(
        data.discord_not
            .adjustment_target(&http, sender, UserId::new(201))
            .await
            .is_err()
    );
    let recipient = data
        .discord_not
        .member_target(&http, sender, UserId::new(201))
        .await
        .unwrap();
    data.sqlite_not
        .adjust_balance(
            admin,
            sender.user_id(),
            domain::Balance::new(1200).unwrap(),
            "seed",
        )
        .await
        .unwrap();
    data.sqlite_not
        .transfer(sender, recipient, 500, "interaction")
        .await
        .unwrap();
    let target = data
        .discord_not
        .adjustment_target(&http, admin, UserId::new(201))
        .await
        .unwrap();
    let penalty = data
        .sqlite_not
        .adjust_amount(
            admin,
            target,
            domain::NotAmount::new(-100),
            "adjustment-interaction",
        )
        .await
        .unwrap();
    assert_eq!(penalty.amount().get(), -100);
    assert_eq!(
        data.sqlite_not.get_balance(sender).await.unwrap().get(),
        700
    );
    mock.deny_member.store(true, Ordering::Relaxed);
    assert!(
        data.discord_not
            .authenticate(&http, GuildId::new(100), UserId::new(200))
            .await
            .is_err()
    );
    assert!(
        data.discord_not
            .member_target(&http, sender, UserId::new(201))
            .await
            .is_err()
    );
    let definition = to_value(super::not().create_as_slash_command().unwrap()).unwrap();
    let options = definition["options"].as_array().unwrap();
    assert!(!options.iter().any(|option| option["name"] == "penalize"));
    let transfer = options
        .iter()
        .find(|option| option["name"] == "transfer")
        .unwrap();
    let amount = transfer["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["name"] == "amount")
        .unwrap();
    let choices: Vec<_> = amount["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|choice| choice["name"].clone())
        .collect();
    assert_eq!(choices, [json!("100"), json!("500"), json!("1000")]);
    data.database.close().await.unwrap();
}
