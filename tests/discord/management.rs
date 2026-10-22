use crate::infrastructure::discord::not::NotRepository as DiscordNotRepository;
use crate::infrastructure::discord::shop::ShopRepository as DiscordShopRepository;
use crate::infrastructure::types as values;
use std::{fs, sync::atomic::Ordering, time::SystemTime};

use serenity::{
    all::{GuildId, InteractionId, UserId},
    json::{Value, from_str, to_value},
};

use crate::infrastructure::{RepositoryError, discord::test_support::*};

#[tokio::test]
async fn 商品管理は最新の公開投稿を編集し公開に失敗しても保存済みの変更を保つ() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    // 過去の投稿ではなく、最新の公開記録を編集する。
    let mut write = data.database.begin_write().await.unwrap();
    write
        .append_publication(
            &item,
            values::channel_id(400).unwrap(),
            values::message_id(901).unwrap(),
        )
        .await
        .unwrap();
    write.commit().await.unwrap();
    let buyer = values::actor(actor().guild_id(), values::user_id(201).unwrap(), false);
    let admin = values::actor(actor().guild_id(), actor().user_id(), true);
    data.sqlite_not
        .adjust_balance(
            admin,
            buyer.user_id(),
            values::balance(1200).unwrap(),
            "fund",
        )
        .await
        .unwrap();
    let quote = data
        .sqlite_shop
        .prepare_purchase(buyer, item.id(), "old-price", SystemTime::now())
        .await
        .unwrap();
    let details =
        DiscordShopRepository::item_details("新しい資料".into(), "変更後".into(), "600").unwrap();
    assert!(
        data.sqlite_shop
            .update(buyer, item.id(), 1, details.clone())
            .await
            .is_err()
    );
    let other_guild = values::actor(values::guild_id(101).unwrap(), actor().user_id(), false);
    assert!(
        data.sqlite_shop
            .delete(other_guild, item.id(), 1)
            .await
            .is_err()
    );
    let different = values::item_details("別の変更", "", values::price(700).unwrap()).unwrap();
    let (a, b) = tokio::join!(
        data.sqlite_shop.update(actor(), item.id(), 1, details),
        data.sqlite_shop.update(actor(), item.id(), 1, different),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let latest = data.sqlite_shop.find(actor(), item.id()).await.unwrap();
    assert_eq!(latest.revision(), 2);
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    assert!(matches!(
        data.sqlite_shop
            .purchase(buyer, &quote, SystemTime::now())
            .await,
        Err(RepositoryError::RevisionConflict)
    ));
    assert_eq!(
        data.sqlite_not.get_balance(buyer).await.unwrap().get(),
        1200
    );
    {
        let requests = mock.requests.lock().unwrap();
        let (_, body) = requests
            .iter()
            .find(|(route, _)| {
                route.starts_with("PATCH ") && route.contains("/channels/400/messages/901")
            })
            .unwrap();
        let payload: Value = from_str(body).unwrap();
        assert_eq!(payload["embeds"][0]["title"], latest.details().name());
        assert!(body.contains(&format!("{} NOT", latest.details().price().get())));
        assert!(!body.contains("private-save-key"));
        assert!(!body.contains("cdn.discordapp.com"));
    }
    mock.fail_post.store(true, Ordering::Relaxed);
    let deleted = data
        .sqlite_shop
        .delete(actor(), item.id(), 2)
        .await
        .unwrap();
    assert!(deleted.is_deleted());
    let (text, buttons) = data
        .publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    assert!(text.contains("PublicationPending"));
    assert!(!text.contains("private-external-error"));
    assert_eq!(buttons.len(), 1);
    assert!(
        data.sqlite_shop
            .prepare_purchase(buyer, item.id(), "after-delete", SystemTime::now())
            .await
            .is_err()
    );
    assert!(
        data.publication_status(&http, buyer, item.id())
            .await
            .is_err()
    );
    mock.fail_post.store(false, Ordering::Relaxed);
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    {
        let requests = mock.requests.lock().unwrap();
        let (_, body) = requests
            .iter()
            .rev()
            .find(|(route, _)| route.starts_with("PATCH "))
            .unwrap();
        let payload: Value = from_str(body).unwrap();
        assert_eq!(
            payload["components"][0]["components"][0]["label"],
            "販売終了"
        );
        assert_eq!(payload["components"][0]["components"][0]["disabled"], true);
    }
    // Discordで投稿が削除されていても、販売終了の表示として復旧する。
    mock.missing_message.store(true, Ordering::Relaxed);
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    let mut read = data.database.begin_read().await.unwrap();
    let publication = read
        .find_publication(actor().guild_id(), item.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((publication.0, publication.2.get()), (3, 900));
    read.commit().await.unwrap();
    assert_eq!(mock.posts().len(), 2);
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 売買と商品管理を経てバックアップを復元しても無料再送と管理者返金ができる() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let buyer = DiscordNotRepository::new(&http)
        .authenticate(GuildId::new(100), UserId::new(201))
        .await
        .unwrap();
    let admin = DiscordNotRepository::new(&http)
        .authenticate(GuildId::new(100), UserId::new(999))
        .await
        .unwrap();
    data.sqlite_not
        .adjust_balance(
            admin,
            buyer.user_id(),
            values::balance(1200).unwrap(),
            "supply",
        )
        .await
        .unwrap();
    let item = saved_item(&data).await;
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    let mut buy = component(&format!("content:buy:100:{}", item.id()), 201);
    buy.guild_id = Some(GuildId::new(100));
    buy.id = InteractionId::new(911);
    data.purchase_action(&http, &buy, SystemTime::now())
        .await
        .unwrap();
    assert_eq!(
        data.sqlite_not.get_balance(buyer).await.unwrap().get(),
        1200
    );
    let yes = component("content:confirm:100-911:yes", 201);
    assert!(
        data.purchase_action(&http, &yes, SystemTime::now())
            .await
            .unwrap()
            .0
            .contains("送信しました")
    );
    let purchase = data
        .sqlite_shop
        .list_purchases(buyer)
        .await
        .unwrap()
        .remove(0)
        .buyer()
        .id();
    let resend = component(&format!("content:resend:100:{purchase}"), 201);
    data.purchase_action(&http, &resend, SystemTime::now())
        .await
        .unwrap();
    assert_eq!(data.sqlite_not.get_balance(buyer).await.unwrap().get(), 700);
    assert_eq!(
        data.sqlite_not.get_balance(actor()).await.unwrap().get(),
        500
    );
    data.sqlite_shop
        .update(
            actor(),
            item.id(),
            1,
            DiscordShopRepository::item_details("改訂版".into(), "".into(), "800").unwrap(),
        )
        .await
        .unwrap();
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    data.sqlite_shop
        .delete(actor(), item.id(), 2)
        .await
        .unwrap();
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    data.database.close().await.unwrap();

    let restored_directory = TestDirectory::new();
    fs::copy(
        directory.0.join("db.sqlite"),
        restored_directory.0.join("db.sqlite"),
    )
    .unwrap();
    fs::create_dir(restored_directory.0.join("private")).unwrap();
    for file in fs::read_dir(&data.discord_shop.directory).unwrap() {
        let file = file.unwrap();
        fs::copy(
            file.path(),
            restored_directory.0.join("private").join(file.file_name()),
        )
        .unwrap();
    }
    let restored = setup(&restored_directory).await;
    assert_eq!(
        restored.sqlite_not.get_balance(buyer).await.unwrap().get(),
        700
    );
    assert!(restored.sqlite_shop.find(actor(), item.id()).await.is_err());
    restored
        .purchase_action(&http, &resend, SystemTime::now())
        .await
        .unwrap();
    let files = mock
        .posts()
        .iter()
        .filter(|body| body.contains("name=\"files[0]\"") && body.contains("pdf"))
        .count();
    assert_eq!(files, 3);
    assert!(
        restored
            .refund_purchase(&http, buyer, purchase, "refund")
            .await
            .is_err()
    );
    assert!(
        restored
            .refund_purchase(&http, admin, purchase, "refund")
            .await
            .unwrap()
            .contains("500 NOT")
    );
    restored
        .refund_purchase(&http, admin, purchase, "refund")
        .await
        .unwrap();
    assert!(
        restored
            .refund_purchase(&http, admin, purchase, "different-refund")
            .await
            .is_err()
    );
    assert_eq!(
        restored.sqlite_not.get_balance(buyer).await.unwrap().get(),
        1200
    );
    assert_eq!(
        restored
            .sqlite_not
            .get_balance(actor())
            .await
            .unwrap()
            .get(),
        0
    );
    let before = mock.posts().len();
    assert!(
        restored
            .purchase_action(&http, &resend, SystemTime::now())
            .await
            .is_err()
    );
    assert!(
        restored
            .purchase_action(&http, &yes, SystemTime::now())
            .await
            .is_err()
    );
    assert_eq!(mock.posts().len(), before);
    assert!(
        restored
            .sqlite_shop
            .list_purchases(buyer)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read_dir(&restored.discord_shop.directory)
            .unwrap()
            .count(),
        1
    );
    // 登録される操作名も、実際に呼ばれるコマンド定義から確認する。
    let definition = to_value(
        super::super::commands::content()
            .create_as_slash_command()
            .unwrap(),
    )
    .unwrap();
    for name in ["update", "delete", "republish", "refund"] {
        assert!(
            definition["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| option["name"] == name)
        );
    }
    restored.database.close().await.unwrap();
}

#[test]
fn 商品版の入力は正の整数に限り不正値を版番号として説明する() {
    assert_eq!(DiscordShopRepository::parse_revision("001").unwrap(), 1);
    for value in ["", "0", "-1", "+1", "１", "9223372036854775808"] {
        let error = DiscordShopRepository::parse_revision(value).unwrap_err();
        assert!(error.to_string().contains("商品版"), "{value}: {error}");
    }
}
