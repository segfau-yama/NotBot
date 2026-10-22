use crate::domain;
use crate::infrastructure::discord::shop::ShopRepository as DiscordShopRepository;
use crate::infrastructure::discord::test_support::actor;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::Ordering,
    thread,
};

use sea_orm::ConnectionTrait;
use serenity::json::{Value, from_value, json, to_value};

use super::super::test_support::*;
use super::*;
use crate::infrastructure::MAX_CONTENT_BYTES;

fn attachment() -> Attachment {
    from_value(json!({"id":"55", "filename":"資料.pdf", "size":3,
        "url":"https://cdn.discordapp.com/attachments/10/55/file.pdf?secret=original-url",
        "proxy_url":""}))
    .unwrap()
}

fn draft(id: &str) -> ListingDraft {
    let discord_shop = DiscordShopRepository::new(Default::default());
    ListingDraft::new(
        &discord_shop,
        id.into(),
        actor(),
        discord_shop
            .create("資料集".into(), "商品説明".into(), "500")
            .unwrap(),
        attachment(),
    )
    .unwrap()
}

#[test]
fn 出品コマンドは本人との会話で商品情報と価格とサーバーと添付を必須にする() {
    let commands =
        poise::builtins::create_application_commands(&[super::super::commands::content()]);
    let command = to_value(&commands[0]).unwrap();
    assert_eq!(command["contexts"], json!([1]));
    assert_eq!(command["integration_types"], json!([0, 1]));
    let options = command["options"][0]["options"].as_array().unwrap();
    assert_eq!(options.len(), 5);
    for option in options {
        assert_eq!(option["required"], true);
    }
    assert!(
        options
            .iter()
            .any(|option| option["name"] == "file" && option["type"] == 11)
    );
    assert!(
        options
            .iter()
            .any(|option| option["name"] == "price" && option["type"] == 3)
    );
    assert!(
        options
            .iter()
            .any(|option| option["name"] == "guild" && option["autocomplete"] == true)
    );
}

#[test]
fn 商品と購入とロールの識別子には正の十進整数だけを指定できる() {
    let discord_shop = DiscordShopRepository::new(Default::default());
    for input in [
        "",
        "0",
        "-1",
        "+1",
        " 1",
        "1 ",
        "1.0",
        "１２",
        "18446744073709551616",
    ] {
        assert!(discord_shop.read(input).is_err(), "{input}");
        assert!(discord_shop.parse_purchase_id(input).is_err(), "{input}");
        assert!(discord_shop.parse_role_id(input).is_err(), "{input}");
    }
    assert_eq!(discord_shop.read("001").unwrap().get(), 1);
    assert_eq!(discord_shop.parse_purchase_id("001").unwrap().get(), 1);
    assert_eq!(discord_shop.parse_role_id("001").unwrap().get(), 1);
    assert!(discord_shop.read("9223372036854775808").is_err());
    assert!(
        discord_shop
            .parse_purchase_id("9223372036854775808")
            .is_err()
    );
    assert_eq!(
        discord_shop
            .parse_role_id("18446744073709551615")
            .unwrap()
            .get(),
        u64::MAX
    );
}

#[test]
fn 出品価格には正の整数だけを指定できる() {
    let discord_shop = DiscordShopRepository::new(Default::default());
    for price in ["0", "-1", "1.5", "9223372036854775808", " 500"] {
        assert!(
            discord_shop
                .create("資料".into(), "".into(), price,)
                .is_err(),
            "{price}"
        );
    }
}

#[test]
fn 出品名は絵文字も文字数制限の対象になる() {
    let discord_shop = DiscordShopRepository::new(Default::default());
    assert!(
        discord_shop
            .create("😀".repeat(129), "".into(), "500",)
            .is_err()
    );
}

#[tokio::test]
async fn 下書きは作成した本人だけが同じ確認から操作できる() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    data.remember_draft(draft("same-id"), "販売先").await;

    for (user, id) in [(201, "same-id"), (200, "different-id")] {
        assert!(
            data.listing_action(
                &http,
                &component(&format!("content:draft:{id}:cancel"), user)
            )
            .await
            .is_err()
        );
    }
    let (text, _) = data
        .listing_action(&http, &component("content:draft:same-id:cancel", 200))
        .await
        .unwrap();
    assert_eq!(text, "出品をキャンセルしました。");
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 同じ下書きの取消は一度だけ受け付ける() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    data.remember_draft(draft("same-id"), "販売先").await;
    let cancel = component("content:draft:same-id:cancel", 200);

    let (text, _) = data.listing_action(&http, &cancel).await.unwrap();
    assert_eq!(text, "出品をキャンセルしました。");
    assert!(data.listing_action(&http, &cancel).await.is_err());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 期限切れの下書きは確定できない() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let mut expired = draft("expired");
    expired.expires_at = Instant::now();
    data.remember_draft(expired, "販売先").await;

    assert!(
        data.listing_action(&http, &component("content:draft:expired:publish", 200))
            .await
            .is_err()
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    data.database.close().await.unwrap();
}

#[test]
fn 添付には外部サイトや危険なファイル名や配送できないサイズを許可しない() {
    let discord_shop = DiscordShopRepository::new(Default::default());
    for url in [
        "http://cdn.discordapp.com/attachments/1/2/a",
        "https://example.com/file",
        "https://cdn.discordapp.com.evil.test/attachments/a",
        "https://cdn.discordapp.com:444/attachments/a",
        "https://cdn.discordapp.com/not-an-attachment",
    ] {
        let mut file = attachment();
        file.url = url.into();
        assert!(discord_shop.validate_attachment(&file).is_err());
    }
    for size in [0, (MAX_CONTENT_BYTES + 1) as u32] {
        let mut file = attachment();
        file.size = size;
        assert!(discord_shop.validate_attachment(&file).is_err());
    }
    let mut file = attachment();
    file.filename = "../secret".into();
    assert!(discord_shop.validate_attachment(&file).is_err());
    assert!(discord_shop.validate_attachment(&attachment()).is_ok());
}

#[tokio::test]
async fn 公開失敗後も同じ商品を再投稿し公開情報に非公開ファイル情報を含めない() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    mock.fail_post.store(true, Ordering::Relaxed);
    let (text, buttons) = data
        .publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    assert!(text.contains("PublicationPending"));
    assert!(text.contains(&item.id().to_string()));
    assert!(!text.contains("private-external-error"));
    assert_eq!(buttons.len(), 1);
    let mut read = data.database.begin_read().await.unwrap();
    assert_eq!(
        read.find_publication(actor().guild_id(), item.id())
            .await
            .unwrap(),
        None
    );
    read.commit().await.unwrap();
    mock.fail_post.store(false, Ordering::Relaxed);
    assert_eq!(
        data.publication_status(&http, actor(), item.id())
            .await
            .unwrap()
            .0,
        format!(
            "公開表示へ反映しました。商品ID: {}\nhttps://discord.com/channels/100/400/900",
            item.id()
        )
    );
    assert_eq!(
        data.publication_status(&http, actor(), item.id())
            .await
            .unwrap()
            .0,
        format!(
            "公開表示へ反映しました。商品ID: {}\nhttps://discord.com/channels/100/400/900",
            item.id()
        )
    );
    assert_eq!(mock.posts().len(), 2);
    assert_eq!(data.sqlite_shop.list(actor()).await.unwrap().len(), 1);
    for body in mock.posts() {
        let payload: Value = serenity::json::from_str(&body).unwrap();
        assert!(
            payload
                .get("attachments")
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
        );
        assert!(!body.contains("private-save-key"));
        assert!(!body.contains("original-url"));
        assert!(!body.contains("cdn.discordapp.com"));
        assert!(!body.contains(&data.discord_shop.directory.to_string_lossy().to_string()));
        assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
        assert_eq!(
            payload["components"][0]["components"][0]["custom_id"],
            format!("content:buy:100:{}", item.id())
        );
        assert_eq!(payload["flags"].as_u64().unwrap_or(0) & (1 << 15), 0);
    }
    let mut read = data.database.begin_read().await.unwrap();
    let record = read
        .find_publication(actor().guild_id(), item.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((record.0, record.1.get(), record.2.get()), (1, 400, 900));
    assert!(
        read.find_publication(domain::GuildId::new(101).unwrap(), item.id())
            .await
            .unwrap()
            .is_none()
    );
    read.commit().await.unwrap();
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 商品を公開できるのは出品者本人で正しい販売先とボットの投稿権限が必要である() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    let stranger = domain::Actor::new(actor().guild_id(), domain::UserId::new(201).unwrap(), false);
    assert!(
        data.publication_status(&http, stranger, item.id())
            .await
            .unwrap_err()
            .is::<RepositoryError>()
    );
    mock.wrong_channel.store(true, Ordering::Relaxed);
    assert!(
        data.publication_status(&http, actor(), item.id())
            .await
            .unwrap()
            .0
            .contains("PublicationPending")
    );
    mock.wrong_channel.store(false, Ordering::Relaxed);
    mock.deny_embed.store(true, Ordering::Relaxed);
    assert!(
        data.publication_status(&http, actor(), item.id())
            .await
            .unwrap()
            .0
            .contains("PublicationPending")
    );
    assert!(mock.posts().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 公開記録の保存に失敗しても商品を増やさず同じ商品を再投稿できる() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    data.database.clone().close().await.unwrap();
    let (text, buttons) = data
        .publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    assert!(text.contains(&format!("PublicationPending: 商品ID {}", item.id())));
    assert_eq!(
        to_value(&buttons).unwrap()[0]["components"][0]["custom_id"],
        format!("content:republish:100:{}", item.id())
    );
    let data = setup(&directory).await;
    let raw = raw(&directory).await;
    raw.execute_unprepared("CREATE TRIGGER reject_publication BEFORE INSERT ON shop_item_publication BEGIN SELECT RAISE(ABORT,'injected'); END;").await.unwrap();
    assert!(
        data.publication_status(&http, actor(), item.id())
            .await
            .unwrap()
            .0
            .contains("PublicationPending")
    );
    raw.execute_unprepared("DROP TRIGGER reject_publication")
        .await
        .unwrap();
    data.publication_status(&http, actor(), item.id())
        .await
        .unwrap();
    assert_eq!(mock.posts().len(), 2);
    assert_eq!(data.sqlite_shop.list(actor()).await.unwrap().len(), 1);
    assert!(
        data.discord_shop
            .directory
            .join("content-100-200-private-save-key")
            .is_file()
    );
    raw.close().await.unwrap();
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 添付は実サイズを確認し上限超過と別の取得先への転送を拒否する() {
    let discord_shop = DiscordShopRepository::new(Default::default());
    for (status, body, expected) in [
        (200, vec![b'x'; 3], 3),
        (200, vec![b'x'; 3], 2),
        (302, vec![], 3),
        (
            200,
            vec![b'x'; MAX_CONTENT_BYTES as usize + 1],
            MAX_CONTENT_BYTES + 1,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            write!(
                stream,
                "HTTP/1.1 {status} Mock\r\nLocation: /redirected\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let _ = stream.write_all(&body);
        });
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
            .get(format!("http://{address}/file"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        let result = discord_shop.read_attachment(response, expected).await;
        if status == 200 && expected == 3 {
            assert_eq!(result.unwrap(), b"xxx");
        } else {
            assert!(result.is_err());
        }
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn 下書きの取消や別人の操作ではファイルも商品も保存しない() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    data.drafts.lock().await.insert(200, draft("preview"));
    assert!(
        data.listing_action(&http, &component("content:draft:preview:publish", 201))
            .await
            .is_err()
    );
    assert!(data.drafts.lock().await.contains_key(&200));
    let (text, _) = data
        .listing_action(&http, &component("content:draft:preview:cancel", 200))
        .await
        .unwrap();
    assert!(text.contains("キャンセル"));
    assert!(
        data.listing_action(&http, &component("content:draft:preview:publish", 200))
            .await
            .is_err()
    );
    assert!(data.sqlite_shop.list(actor()).await.unwrap().is_empty());
    assert!(!data.discord_shop.directory.exists());
    assert!(mock.requests.lock().unwrap().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 再起動後も出品者を認証し同じ商品の同時再投稿では公開先を共有する() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let item = saved_item(&data).await;
    data.database.close().await.unwrap();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let click = component(&format!("content:republish:100:{}", item.id()), 200);
    let (first, second) = tokio::join!(
        data.listing_action(&http, &click),
        data.listing_action(&http, &click)
    );
    assert!(first.unwrap().0.contains("公開表示へ反映しました"));
    assert!(second.unwrap().0.contains("公開表示へ反映しました"));
    assert_eq!(mock.posts().len(), 1);
    assert_eq!(data.sqlite_shop.list(actor()).await.unwrap().len(), 1);
    assert!(data.drafts.lock().await.is_empty());
    data.database.close().await.unwrap();
}
