use crate::infrastructure::discord::not::NotRepository as DiscordNotRepository;
use crate::infrastructure::types as values;
use std::sync::atomic::Ordering;

use serenity::json::to_value;

use super::*;
use crate::infrastructure::{
    RepositoryError,
    discord::{commands, events, test_support::*},
};

#[tokio::test]
async fn 所属と権限を確認した本人だけが対象サーバーの残高とランキングを利用できる() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let a = DiscordNotRepository::target_guild(Some(GuildId::new(100)), None).unwrap();
    let b = DiscordNotRepository::target_guild(None, Some("101")).unwrap();
    let admin = DiscordNotRepository::new(&http)
        .authenticate(a, UserId::new(999))
        .await
        .unwrap();
    let viewer = DiscordNotRepository::new(&http)
        .authenticate(a, UserId::new(200))
        .await
        .unwrap();
    let other_guild_viewer = DiscordNotRepository::new(&http)
        .authenticate(b, UserId::new(200))
        .await
        .unwrap();
    let target = DiscordNotRepository::new(&http)
        .adjustment_target(admin, UserId::new(200))
        .await
        .unwrap();
    let balance = values::parse_balance("1200").unwrap();
    let entry = data
        .sqlite_not
        .adjust_balance(admin, target, balance, "interaction-700")
        .await
        .unwrap();
    assert_eq!(
        data.sqlite_not
            .adjust_balance(admin, target, balance, "interaction-700")
            .await
            .unwrap(),
        entry
    );
    assert_eq!(data.sqlite_not.get_balance(viewer).await.unwrap(), balance);
    let dm = DiscordNotRepository::new(&http)
        .authenticate(
            DiscordNotRepository::target_guild(None, Some("100")).unwrap(),
            UserId::new(200),
        )
        .await
        .unwrap();
    assert_eq!(data.sqlite_not.get_balance(dm).await.unwrap(), balance);
    assert_eq!(
        data.sqlite_not
            .get_balance(other_guild_viewer)
            .await
            .unwrap(),
        values::balance(0).unwrap()
    );
    assert_eq!(
        data.sqlite_not.get_ranking(viewer, 10).await.unwrap()[0].user_id(),
        target
    );
    assert!(
        data.sqlite_not
            .get_ranking(other_guild_viewer, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        DiscordNotRepository::new(&http)
            .adjustment_target(viewer, UserId::new(200))
            .await
            .is_err()
    );
    assert_eq!(
        data.sqlite_not
            .adjust_balance(viewer, target, balance, "forbidden")
            .await,
        Err(RepositoryError::PermissionDenied)
    );
    let mut guild = http.get_guild(a).await.unwrap();
    let member = http.get_member(a, UserId::new(200)).await.unwrap();
    guild.roles.get_mut(&RoleId::new(100)).unwrap().permissions = Permissions::MANAGE_GUILD;
    assert!(
        !DiscordNotRepository::authenticated_actor(&guild, &member, member.user.id)
            .unwrap()
            .can_manage_not()
    );
    guild.roles.get_mut(&RoleId::new(100)).unwrap().permissions = Permissions::ADMINISTRATOR;
    assert!(
        DiscordNotRepository::authenticated_actor(&guild, &member, member.user.id)
            .unwrap()
            .can_manage_not()
    );
    guild.roles.get_mut(&RoleId::new(100)).unwrap().permissions = Permissions::empty();
    assert!(
        !DiscordNotRepository::authenticated_actor(&guild, &member, member.user.id)
            .unwrap()
            .can_manage_not()
    );
    assert!(DiscordNotRepository::authenticated_actor(&guild, &member, UserId::new(201)).is_err());
    mock.deny_member.store(true, Ordering::Relaxed);
    assert!(
        DiscordNotRepository::new(&http)
            .authenticate(a, UserId::new(200))
            .await
            .is_err()
    );
    assert!(
        DiscordNotRepository::new(&http)
            .adjustment_target(admin, UserId::new(200))
            .await
            .is_err()
    );
    mock.deny_member.store(false, Ordering::Relaxed);
    assert!(
        DiscordNotRepository::new(&http)
            .authenticate(GuildId::new(102), UserId::new(200))
            .await
            .is_err()
    );
    data.database.close().await.unwrap();
}

#[test]
fn サーバー内から別サーバーは指定できず個別の会話では指定が必要である() {
    assert!(DiscordNotRepository::target_guild(None, None).is_err());
    assert!(DiscordNotRepository::target_guild(Some(GuildId::new(100)), Some("101")).is_err());
    assert!(DiscordNotRepository::target_guild(None, Some("0")).is_err());
}

#[test]
fn 残高の入力は64ビットの非負整数に限る() {
    assert_eq!(
        values::parse_balance("9223372036854775807").unwrap().get(),
        i64::MAX
    );
    for value in ["", "-1", "1.0", "1e3", "１２", "9223372036854775808"] {
        assert!(values::parse_balance(value).is_err(), "{value}");
    }
}

#[test]
fn 増減額の入力は符号付き六十四ビット整数として検証する() {
    for (input, expected) in [
        ("-9223372036854775808", i64::MIN),
        ("9223372036854775807", i64::MAX),
        ("0", 0),
        ("+500", 500),
        ("-500", -500),
    ] {
        assert_eq!(
            DiscordNotRepository::parse_amount(input).unwrap().get(),
            expected
        );
    }
    for input in [
        "",
        " 1",
        "1 ",
        "1.0",
        "1e3",
        "１２",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        assert!(
            DiscordNotRepository::parse_amount(input).is_err(),
            "{input}"
        );
    }
}

#[test]
fn 残高コマンドは対象サーバーと増減額を文字列で受け取る() {
    let definition = to_value(commands::not().create_as_slash_command().unwrap()).unwrap();
    assert_eq!(definition["integration_types"], serenity::json::json!([0]));
    assert_eq!(definition["contexts"], serenity::json::json!([0, 1]));
    for name in ["balance", "ranking", "adjust"] {
        let command = definition["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["name"] == name)
            .unwrap();
        let options = command["options"].as_array().unwrap();
        let guild = options
            .iter()
            .find(|option| option["name"] == "guild")
            .unwrap();
        assert_eq!(guild["type"], 3);
        assert_eq!(guild["autocomplete"], true);
        if name == "adjust" {
            let amount = options
                .iter()
                .find(|option| option["name"] == "amount")
                .unwrap();
            assert_eq!(amount["type"], 3);
            assert_eq!(amount["required"], true);
            assert_eq!(amount["max_length"], 20);
            assert_eq!(
                options
                    .iter()
                    .find(|option| option["name"] == "user")
                    .unwrap()["type"],
                6
            );
        }
    }
}

#[tokio::test]
async fn 返信には外部エラーの秘密情報を含めず業務エラーを表示する() {
    let mock = MockDiscord::new();
    let error: Error = mock
        .http()
        .get_original_interaction_response("offline-secret-interaction-token")
        .await
        .unwrap_err()
        .into();
    let public = events::public_error_message(&error);
    assert!(!public.contains("unknown-route"));
    assert!(!public.contains(&mock.address));
    assert!(!public.contains("offline-secret-interaction-token"));
    let internal: Error = crate::infrastructure::RepositoryError::PermissionDenied.into();
    assert_eq!(
        events::public_error_message(&internal),
        "この操作を行う権限がありません"
    );
}
