use std::{fs, sync::atomic::Ordering, time::Duration};

use sea_orm::ConnectionTrait;
use serenity::json::to_value;

use super::*;
use crate::{
    domain::{Balance, ItemDetails, Price, ShopItem},
    infrastructure::discord::{events, test_support::*},
};

fn buyer() -> Actor {
    Actor::new(
        actor().guild_id(),
        crate::domain::UserId::try_from(201).unwrap(),
        false,
    )
}

async fn scenario() -> (TestDirectory, Data, ShopItem) {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    let item = saved_item(&data).await;
    let admin = Actor::new(actor().guild_id(), actor().user_id(), true);
    data.not
        .adjust_balance(
            admin,
            buyer().user_id(),
            Balance::try_from(1200).unwrap(),
            "fund",
        )
        .await
        .unwrap();
    (directory, data, item)
}

async fn offer(http: &Http, data: &Data, item: &ShopItem, request: u64) -> Result<String, Error> {
    let mut click = component(&format!("content:buy:100:{}", item.id()), 201);
    click.guild_id = Some(GuildId::new(100));
    click.id = InteractionId::new(request);
    handle_action(http, data, &click, SystemTime::now()).await?;
    Ok(format!("100-{request}"))
}

async fn act(http: &Http, data: &Data, id: &str) -> Result<(String, Vec<CreateActionRow>), Error> {
    handle_action(http, data, &component(id, 201), SystemTime::now()).await
}

#[tokio::test]
async fn confirmation_defers_cancels_and_charges_once_even_after_restart_and_deletion() {
    let (directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let mut click = component(&format!("content:buy:100:{}", item.id()), 201);
    click.guild_id = Some(GuildId::new(100));
    events::handle_component(&http, &data, &click)
        .await
        .unwrap();
    assert!(mock.requests.lock().unwrap()[0].0.contains("/callback"));
    let dm = mock
        .posts()
        .into_iter()
        .find(|body| body.contains("購入の確認"))
        .unwrap();
    for expected in [
        "テストサーバー",
        "1200 NOT",
        "700 NOT",
        "はい（500 NOTで購入）",
    ] {
        assert!(dm.contains(expected), "{expected}");
    }
    act(&http, &data, "content:confirm:100-910:no")
        .await
        .unwrap();
    assert!(
        act(&http, &data, "content:confirm:100-910:yes")
            .await
            .is_err()
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);

    let id = offer(&http, &data, &item, 911).await.unwrap();
    let yes = format!("content:confirm:{id}:yes");
    assert!(
        handle_action(&http, &data, &component(&yes, 202), SystemTime::now())
            .await
            .is_err()
    );
    assert!(data.confirmations.lock().await.contains_key(&id));
    let (first, second) = tokio::join!(act(&http, &data, &yes), act(&http, &data, &yes));
    assert!(first.is_ok() || second.is_ok());
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    assert_eq!(data.not.get_balance(actor()).await.unwrap().get(), 500);
    data.shop
        .delete(actor(), item.id(), item.revision())
        .await
        .unwrap();
    data.database.close().await.unwrap();

    let data = setup(&directory).await;
    assert!(
        act(&http, &data, &yes)
            .await
            .unwrap()
            .0
            .contains("送信しました")
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    let purchase = data.shop.list_purchases(buyer()).await.unwrap().remove(0);
    let resend = format!("content:resend:100:{}", purchase.buyer().id());
    assert!(
        handle_action(&http, &data, &component(&resend, 202), SystemTime::now())
            .await
            .is_err()
    );
    let admin = Actor::new(actor().guild_id(), actor().user_id(), true);
    data.shop
        .refund(admin, purchase.buyer().id(), "refund")
        .await
        .unwrap();
    let posts = mock.posts().len();
    assert!(act(&http, &data, &resend).await.is_err());
    assert!(act(&http, &data, &yes).await.is_err());
    assert_eq!(mock.posts().len(), posts);
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn dm_refusal_missing_file_membership_expiry_and_size_limit_never_charge() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    mock.deny_dm.store(true, Ordering::Relaxed);
    assert!(offer(&http, &data, &item, 920).await.is_err());
    assert!(data.confirmations.lock().await.is_empty());
    mock.deny_dm.store(false, Ordering::Relaxed);

    let id = offer(&http, &data, &item, 921).await.unwrap();
    let ItemTarget::Content(file) = item.target() else {
        unreachable!()
    };
    let path = data.content_dir.join(file.key());
    let saved = path.with_extension("backup");
    fs::rename(&path, &saved).unwrap();
    assert!(
        act(&http, &data, &format!("content:confirm:{id}:yes"))
            .await
            .is_err()
    );
    fs::rename(&saved, &path).unwrap();

    let id = offer(&http, &data, &item, 922).await.unwrap();
    mock.deny_member.store(true, Ordering::Relaxed);
    assert!(
        act(&http, &data, &format!("content:confirm:{id}:yes"))
            .await
            .is_err()
    );
    mock.deny_member.store(false, Ordering::Relaxed);

    let id = offer(&http, &data, &item, 923).await.unwrap();
    let mut click = component(&format!("content:confirm:{id}:yes"), 201);
    click.attachment_size_limit = 2;
    assert!(
        handle_action(&http, &data, &click, SystemTime::now())
            .await
            .is_err()
    );

    let id = offer(&http, &data, &item, 924).await.unwrap();
    assert!(
        handle_action(
            &http,
            &data,
            &component(&format!("content:confirm:{id}:yes"), 201),
            SystemTime::now() + Duration::from_secs(301)
        )
        .await
        .is_err()
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
    assert_eq!(data.not.get_balance(actor()).await.unwrap().get(), 0);
    assert!(data.shop.list_purchases(buyer()).await.unwrap().is_empty());
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn delivery_failure_recovers_for_free_and_purchase_pages_do_not_expose_storage() {
    let (directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let id = offer(&http, &data, &item, 930).await.unwrap();
    mock.fail_delivery.store(true, Ordering::Relaxed);
    let (text, buttons) = act(&http, &data, &format!("content:confirm:{id}:yes"))
        .await
        .unwrap();
    assert!(text.contains("DeliveryPending"));
    assert!(!text.contains("private-external-error"));
    let resend = to_value(&buttons).unwrap()[0]["components"][0]["custom_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    data.database.close().await.unwrap();

    let data = setup(&directory).await;
    mock.fail_delivery.store(false, Ordering::Relaxed);
    assert!(
        act(&http, &data, &resend)
            .await
            .unwrap()
            .0
            .contains("送信しました")
    );
    let (text, buttons) = prepare(
        &http,
        &data,
        buyer(),
        item.id(),
        "100-931".into(),
        SystemTime::now(),
    )
    .await
    .unwrap();
    assert!(text.contains("購入済み"));
    assert_eq!(
        to_value(&buttons).unwrap()[0]["components"][0]["custom_id"],
        resend
    );
    assert!(data.confirmations.lock().await.is_empty());
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    let files: Vec<_> = mock
        .posts()
        .into_iter()
        .filter(|body| body.contains("name=\"files[0]\""))
        .collect();
    assert_eq!(files.len(), 2);
    for body in files {
        assert!(body.contains("pdf"));
        assert!(!body.contains("private-save-key"));
    }
    for request in 0..6 {
        let stored = content::store_content(
            &data.content_dir,
            actor(),
            &format!("page-{request}"),
            "a.txt",
            &b"x"[..],
        )
        .unwrap();
        let details = ItemDetails::try_new("商品", "", Price::try_from(1).unwrap()).unwrap();
        let item = data.shop.create(actor(), details, stored).await.unwrap();
        let quote = data
            .shop
            .prepare_purchase(
                buyer(),
                item.id(),
                &format!("page-{request}"),
                SystemTime::now(),
            )
            .await
            .unwrap();
        data.shop
            .purchase(buyer(), &quote, SystemTime::now())
            .await
            .unwrap();
    }
    let (text, buttons) = list(&http, &data, buyer(), 0).await.unwrap();
    assert!(text.starts_with("サーバー: テストサーバー\n購入一覧"));
    assert!(text.contains("1/2"));
    assert_eq!(
        to_value(&buttons).unwrap()[0]["components"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let (text, _) = act(&http, &data, "content:purchases:100:1").await.unwrap();
    assert!(text.starts_with("サーバー: テストサーバー\n購入一覧"));
    assert!(text.contains("2/2"));
    assert!(!text.contains("content-"));
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 694);
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn payment_failure_rolls_back_and_preserves_the_same_confirmation_recovery_button() {
    let (directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let id = offer(&http, &data, &item, 940).await.unwrap();
    let raw = raw(&directory).await;
    raw.execute_unprepared("CREATE TRIGGER reject_sale BEFORE INSERT ON not_transaction WHEN NEW.reason='SALE' BEGIN SELECT RAISE(ABORT,'injected'); END").await.unwrap();
    let (text, buttons) = act(&http, &data, &format!("content:confirm:{id}:yes"))
        .await
        .unwrap();
    assert!(text.contains(&id));
    assert_eq!(
        to_value(&buttons).unwrap()[0]["components"][0]["custom_id"],
        format!("content:confirm:{id}:yes")
    );
    raw.execute_unprepared("DROP TRIGGER reject_sale")
        .await
        .unwrap();
    assert!(
        act(&http, &data, &format!("content:confirm:{id}:yes"))
            .await
            .is_err()
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
    assert_eq!(data.not.get_balance(actor()).await.unwrap().get(), 0);
    raw.close().await.unwrap();
    data.database.close().await.unwrap();
}
