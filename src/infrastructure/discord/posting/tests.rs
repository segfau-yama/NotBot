use serenity::json::{from_value, json, to_value};

use super::*;
use crate::infrastructure::discord::test_support::*;

fn upload(id: u64) -> Message {
    let mut value = message();
    value["id"] = json!(id.to_string());
    value["channel_id"] = json!("700");
    value["author"]["id"] = json!("200");
    value["author"]["bot"] = json!(false);
    value["attachments"] = json!([{"id":"55","filename":"資料.pdf","size":3,
        "url":"https://cdn.discordapp.com/attachments/10/55/file.pdf?secret=original-url","proxy_url":""}]);
    from_value(value).unwrap()
}

fn modal(id: u64, guild: u64, user: u64, price: &str) -> ModalInteraction {
    from_value(json!({"id":"930","application_id":"42","channel_id":"700",
    "user":{"id":user.to_string(),"username":"seller","discriminator":"0","avatar":null},
    "token":"offline-interaction-token","version":1,"locale":"ja","entitlements":[],
    "attachment_size_limit":20971520,
    "data":{"custom_id":format!("content:post:{id}:form:{guild}"),"components":[
        {"type":1,"components":[{"type":4,"custom_id":"title","value":"資料集"}]},
        {"type":1,"components":[{"type":4,"custom_id":"description","value":"商品説明"}]},
        {"type":1,"components":[{"type":4,"custom_id":"price","value":price}]}
    ]}}))
    .unwrap()
}

#[tokio::test]
async fn dm_attachment_selection_and_modal_join_the_same_unpublished_draft() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let file = upload(950);
    receive(&http, &data, &file).await.unwrap();
    assert!(mock.posts()[0].contains("このファイルを出品する"));
    let (_, rows) = servers(&http, &data, file.author.id, "950", 0)
        .await
        .unwrap();
    assert_eq!(
        to_value(&rows).unwrap()[0]["components"][0]["options"][0]["value"],
        "100"
    );

    let mut select = component("content:post:950:server", 200);
    select.data.kind = ComponentInteractionDataKind::StringSelect {
        values: vec!["100".into()],
    };
    let count = mock.requests.lock().unwrap().len();
    handle_component(&http, &data, &select).await.unwrap();
    {
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), count + 1);
        assert_eq!(
            serenity::json::from_str::<serenity::json::Value>(&requests[count].1).unwrap()["type"],
            9
        );
    }

    handle_modal(&http, &data, &modal(950, 100, 200, "500"))
        .await
        .unwrap();
    assert!(data.uploads.lock().await.is_empty());
    let expected = ListingDraft::new(
        "950".into(),
        actor(),
        "資料集".into(),
        "商品説明".into(),
        "500",
        file.attachments[0].clone(),
    )
    .unwrap();
    let drafts = data.drafts.lock().await;
    assert_eq!(
        to_value(
            drafts
                .get(&200)
                .unwrap()
                .preview("テストサーバー")
                .to_slash_initial_response_edit(EditInteractionResponse::new())
        )
        .unwrap(),
        to_value(
            expected
                .preview("テストサーバー")
                .to_slash_initial_response_edit(EditInteractionResponse::new())
        )
        .unwrap()
    );
    drop(drafts);
    assert!(!data.content_dir.exists());
    assert!(data.shop.list(actor()).await.unwrap().is_empty());
    listings::handle_action(&http, &data, &component("content:draft:950:cancel", 200))
        .await
        .unwrap();
    assert!(data.drafts.lock().await.is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn input_rejects_foreign_stale_cancelled_expired_and_restarted_forms_without_saving() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    receive(&http, &data, &upload(951)).await.unwrap();
    assert!(
        form(&data, UserId::new(201), "951", Some("100"))
            .await
            .is_err()
    );
    let _ = form(&data, UserId::new(200), "951", Some("100"))
        .await
        .unwrap();
    assert!(
        preview(&http, &data, &modal(951, 100, 201, "500"))
            .await
            .is_err()
    );
    assert!(
        preview(&http, &data, &modal(951, 100, 200, "0"))
            .await
            .is_err()
    );
    assert!(
        preview(&http, &data, &modal(951, 101, 200, "500"))
            .await
            .is_err()
    );
    receive(&http, &data, &upload(952)).await.unwrap();
    assert!(
        preview(&http, &data, &modal(951, 100, 200, "500"))
            .await
            .is_err()
    );
    action(&http, &data, &component("content:post:952:cancel", 200))
        .await
        .unwrap();
    assert!(
        form(&data, UserId::new(200), "952", Some("100"))
            .await
            .is_err()
    );
    receive(&http, &data, &upload(953)).await.unwrap();
    data.uploads.lock().await.get_mut(&200).unwrap().expires_at = Instant::now();
    assert!(
        form(&data, UserId::new(200), "953", Some("100"))
            .await
            .is_err()
    );
    receive(&http, &data, &upload(954)).await.unwrap();
    let _ = form(&data, UserId::new(200), "954", Some("100"))
        .await
        .unwrap();
    data.database.close().await.unwrap();
    let data = setup(&directory).await;
    assert!(
        preview(&http, &data, &modal(954, 100, 200, "500"))
            .await
            .is_err()
    );
    assert!(!data.content_dir.exists());
    assert!(data.shop.list(actor()).await.unwrap().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn server_pages_filter_membership_and_handle_empty_pages() {
    let directory = TestDirectory::new();
    let mut data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let mut start = component("content:post:100", 200);
    start.guild_id = Some(GuildId::new(100));
    assert!(action(&http, &data, &start).await.is_err());
    assert!(data.uploads.lock().await.is_empty());
    receive(&http, &data, &upload(955)).await.unwrap();
    assert_eq!(data.uploads.lock().await.get(&200).unwrap().guild, None);
    // 共用HTTPでは100・101に所属しているため、未所属候補は102以降。
    for id in 102..127 {
        data.shop_channels.insert(
            DomainGuildId::try_from(id).unwrap(),
            crate::domain::ChannelId::try_from(400).unwrap(),
        );
    }
    let (_, rows) = servers(&http, &data, UserId::new(200), "955", 0)
        .await
        .unwrap();
    assert_eq!(
        to_value(&rows).unwrap()[0]["components"][0]["options"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (text, rows) = servers(&http, &data, UserId::new(200), "955", 1)
        .await
        .unwrap();
    assert!(text.contains("2/2"));
    assert_eq!(rows.len(), 1);
    mock.deny_member
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = form(&data, UserId::new(200), "955", Some("100"))
        .await
        .unwrap();
    assert!(
        preview(&http, &data, &modal(955, 100, 200, "500"))
            .await
            .is_err()
    );
    assert!(!data.content_dir.exists());
    data.database.close().await.unwrap();
}
