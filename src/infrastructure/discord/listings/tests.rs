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
    ListingDraft::new(
        id.into(),
        actor(),
        "資料集".into(),
        "商品説明".into(),
        "500",
        attachment(),
    )
    .unwrap()
}

#[test]
fn content_command_has_dm_context_and_validated_attachment_and_integer_string_options() {
    let commands =
        poise::builtins::create_application_commands(&[super::super::commands::content()]);
    let command = to_value(&commands[0]).unwrap();
    assert_eq!(command["contexts"], json!([1]));
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
fn drafts_validate_input_ownership_expiry_and_single_acceptance() {
    for price in ["0", "-1", "1.5", "9223372036854775808", " 500"] {
        assert!(
            ListingDraft::new(
                "1".into(),
                actor(),
                "資料".into(),
                "".into(),
                price,
                attachment()
            )
            .is_err()
        );
    }
    assert!(
        ListingDraft::new(
            "1".into(),
            actor(),
            "😀".repeat(129),
            "".into(),
            "500",
            attachment()
        )
        .is_err()
    );
    let mut drafts = HashMap::from([(200, draft("same-id"))]);
    assert!(take_draft(&mut drafts, 201, "same-id").is_err());
    assert!(take_draft(&mut drafts, 200, "different-id").is_err());
    let accepted = take_draft(&mut drafts, 200, "same-id").unwrap();
    assert_eq!(accepted.id, "same-id");
    assert!(take_draft(&mut drafts, 200, "same-id").is_err());
    let mut expired = draft("expired");
    expired.expires_at = Instant::now();
    drafts.insert(200, expired);
    assert!(take_draft(&mut drafts, 200, "expired").is_err());
}

#[test]
fn attachment_metadata_rejects_external_urls_paths_and_undeliverable_sizes() {
    for url in [
        "http://cdn.discordapp.com/attachments/1/2/a",
        "https://example.com/file",
        "https://cdn.discordapp.com.evil.test/attachments/a",
        "https://cdn.discordapp.com:444/attachments/a",
        "https://cdn.discordapp.com/not-an-attachment",
    ] {
        let mut file = attachment();
        file.url = url.into();
        assert!(content::validate_attachment(&file).is_err());
    }
    for size in [0, (MAX_CONTENT_BYTES + 1) as u32] {
        let mut file = attachment();
        file.size = size;
        assert!(content::validate_attachment(&file).is_err());
    }
    let mut file = attachment();
    file.filename = "../secret".into();
    assert!(content::validate_attachment(&file).is_err());
    assert!(content::validate_attachment(&attachment()).is_ok());
}

#[tokio::test]
async fn publication_failure_retries_existing_item_and_never_exposes_content() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    mock.fail_post.store(true, Ordering::Relaxed);
    let (text, buttons) = publication_status(&http, &data, actor(), item.id())
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
        publish(&http, &data, actor(), item.id())
            .await
            .unwrap()
            .get(),
        900
    );
    assert_eq!(
        publish(&http, &data, actor(), item.id())
            .await
            .unwrap()
            .get(),
        900
    );
    assert_eq!(mock.posts().len(), 2);
    assert_eq!(data.shop.list(actor()).await.unwrap().len(), 1);
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
        assert!(!body.contains(&data.content_dir.to_string_lossy().to_string()));
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
        read.find_publication(crate::domain::GuildId::try_from(101).unwrap(), item.id())
            .await
            .unwrap()
            .is_none()
    );
    read.commit().await.unwrap();
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn publication_requires_owner_correct_channel_and_bot_permissions() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    let stranger = Actor::new(
        actor().guild_id(),
        crate::domain::UserId::try_from(201).unwrap(),
        false,
    );
    assert!(
        publish(&http, &data, stranger, item.id())
            .await
            .unwrap_err()
            .is::<UseCaseError>()
    );
    mock.wrong_channel.store(true, Ordering::Relaxed);
    assert!(
        publish(&http, &data, actor(), item.id())
            .await
            .unwrap_err()
            .is::<PublicationPending>()
    );
    mock.wrong_channel.store(false, Ordering::Relaxed);
    mock.deny_embed.store(true, Ordering::Relaxed);
    assert!(
        publish(&http, &data, actor(), item.id())
            .await
            .unwrap_err()
            .is::<PublicationPending>()
    );
    assert!(mock.posts().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn publication_database_failures_keep_retry_and_existing_item() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let item = saved_item(&data).await;
    data.database.clone().close().await.unwrap();
    let (text, buttons) = publication_status(&http, &data, actor(), item.id())
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
        publish(&http, &data, actor(), item.id())
            .await
            .unwrap_err()
            .is::<PublicationPending>()
    );
    raw.execute_unprepared("DROP TRIGGER reject_publication")
        .await
        .unwrap();
    publish(&http, &data, actor(), item.id()).await.unwrap();
    assert_eq!(mock.posts().len(), 2);
    assert_eq!(data.shop.list(actor()).await.unwrap().len(), 1);
    assert!(
        data.content_dir
            .join("content-100-200-private-save-key")
            .is_file()
    );
    raw.close().await.unwrap();
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn attachment_reads_enforce_actual_size_and_reject_redirects() {
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
        let response = download_client()
            .unwrap()
            .get(format!("http://{address}/file"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        let result = content::read_attachment(response, expected).await;
        if status == 200 && expected == 3 {
            assert_eq!(result.unwrap(), b"xxx");
        } else {
            assert!(result.is_err());
        }
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn draft_cancel_and_foreign_click_never_save_or_create_items() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    data.drafts.lock().await.insert(200, draft("preview"));
    assert!(
        handle_action(
            &http,
            &data,
            &component("content:draft:preview:publish", 201)
        )
        .await
        .is_err()
    );
    assert!(data.drafts.lock().await.contains_key(&200));
    let (text, _) = handle_action(
        &http,
        &data,
        &component("content:draft:preview:cancel", 200),
    )
    .await
    .unwrap();
    assert!(text.contains("キャンセル"));
    assert!(
        handle_action(
            &http,
            &data,
            &component("content:draft:preview:publish", 200)
        )
        .await
        .is_err()
    );
    assert!(data.shop.list(actor()).await.unwrap().is_empty());
    assert!(!data.content_dir.exists());
    assert!(mock.requests.lock().unwrap().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn republish_after_restart_authenticates_and_concurrent_retries_share_one_post() {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let item = saved_item(&data).await;
    data.database.close().await.unwrap();
    let data = setup(&directory).await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let click = component(&format!("content:republish:100:{}", item.id()), 200);
    let (first, second) = tokio::join!(
        handle_action(&http, &data, &click),
        handle_action(&http, &data, &click)
    );
    assert!(first.unwrap().0.contains("公開表示へ反映しました"));
    assert!(second.unwrap().0.contains("公開表示へ反映しました"));
    assert_eq!(mock.posts().len(), 1);
    assert_eq!(data.shop.list(actor()).await.unwrap().len(), 1);
    assert!(data.drafts.lock().await.is_empty());
    data.database.close().await.unwrap();
}
