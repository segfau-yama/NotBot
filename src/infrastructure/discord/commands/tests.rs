use std::sync::atomic::Ordering;

use serenity::{
    all::{GuildId, UserId},
    json::{json, to_value},
};

use super::*;
use crate::{domain::Balance, infrastructure::discord::test_support::*};

#[tokio::test]
async fn not_operations_check_current_membership_authority_and_expose_fixed_transfer_choices() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let sender = services::authenticate(&http, GuildId::new(100), UserId::new(200))
        .await
        .unwrap();
    let admin = services::authenticate(&http, GuildId::new(100), UserId::new(999))
        .await
        .unwrap();
    assert!(
        services::adjustment_target(&http, sender, UserId::new(201))
            .await
            .is_err()
    );
    let recipient = services::member_target(&http, sender, UserId::new(201))
        .await
        .unwrap();
    data.not
        .adjust_balance(
            admin,
            sender.user_id(),
            Balance::try_from(1200).unwrap(),
            "seed",
        )
        .await
        .unwrap();
    data.not
        .transfer(sender, recipient, 500, "interaction")
        .await
        .unwrap();
    let target = services::adjustment_target(&http, admin, UserId::new(201))
        .await
        .unwrap();
    let penalty = data
        .not
        .adjust_amount(
            admin,
            target,
            services::parse_amount("-100").unwrap(),
            "adjustment-interaction",
        )
        .await
        .unwrap();
    assert_eq!(penalty.amount().get(), -100);
    assert_eq!(data.not.get_balance(sender).await.unwrap().get(), 700);
    mock.deny_member.store(true, Ordering::Relaxed);
    assert!(
        services::authenticate(&http, GuildId::new(100), UserId::new(200))
            .await
            .is_err()
    );
    assert!(
        services::member_target(&http, sender, UserId::new(201))
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
