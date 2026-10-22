use crate::domain;
use crate::infrastructure::discord::Data;
use crate::infrastructure::discord::test_support::actor;
use std::{sync::atomic::Ordering, time::SystemTime};

use serenity::json::{json, to_value};

use super::*;
use crate::infrastructure::discord::test_support::*;

fn seller() -> Actor {
    domain::Actor::new(actor().guild_id(), domain::UserId::new(999).unwrap(), true)
}
fn buyer() -> Actor {
    domain::Actor::new(actor().guild_id(), domain::UserId::new(201).unwrap(), false)
}
async fn scenario() -> (TestDirectory, Data, ShopItem) {
    let directory = TestDirectory::new();
    let data = setup(&directory).await;
    data.sqlite_not
        .adjust_balance(
            seller(),
            buyer().user_id(),
            domain::Balance::new(1200).unwrap(),
            "fund",
        )
        .await
        .unwrap();
    let item = data
        .sqlite_shop
        .create_role(
            seller(),
            domain::RoleId::new(500).unwrap(),
            data.discord_shop
                .create("会員権".into(), "会員ロール".into(), "500")
                .unwrap(),
        )
        .await
        .unwrap();
    (directory, data, item)
}
async fn offer(http: &Http, data: &Data, item: &ShopItem, id: u64) -> Result<(), Error> {
    let mut click = component(&format!("content:buy:100:{}", item.id()), 201);
    click.guild_id = Some(GuildId::new(100));
    click.id = InteractionId::new(id);
    data.purchase_action(http, &click, SystemTime::now())
        .await?;
    Ok(())
}
async fn confirm(
    http: &Http,
    data: &Data,
    id: u64,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    data.purchase_action(
        http,
        &component(&format!("content:confirm:100-{id}:yes"), 201),
        SystemTime::now(),
    )
    .await
}

#[tokio::test]
async fn ロール販売は管理権限と階層を確認し売買後の返金では先にロールを解除する() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    assert!(
        data.discord_not
            .authenticate(&http, GuildId::new(100), UserId::new(999))
            .await
            .unwrap()
            .can_manage_not()
    );
    let role = domain::RoleId::new(500).unwrap();
    assert!(
        data.sqlite_shop
            .create_role(actor(), role, item.details().clone())
            .await
            .is_err()
    );
    assert_eq!(
        data.sqlite_shop
            .create_role(seller(), role, item.details().clone())
            .await
            .unwrap(),
        item
    );
    assert_eq!(
        data.sqlite_shop
            .prepare_purchase(seller(), item.id(), "self", SystemTime::now())
            .await,
        Err(RepositoryError::InsufficientBalance)
    );
    for (index, field, value) in [
        (0, "managed", json!(true)),
        (0, "position", json!(3)),
        (1, "permissions", json!("0")),
    ] {
        let old = mock.role_definitions.lock().unwrap()[index][field].clone();
        mock.role_definitions.lock().unwrap()[index][field] = value;
        assert!(offer(&http, &data, &item, 910).await.is_err());
        assert_eq!(
            data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
            1200
        );
        mock.role_definitions.lock().unwrap()[index][field] = old;
    }
    assert!(
        Data::check_role(&http, GuildId::new(100), RoleId::new(100))
            .await
            .is_err()
    );
    mock.role_members.lock().unwrap().insert((100, 201));
    assert!(offer(&http, &data, &item, 910).await.is_err());
    mock.role_members.lock().unwrap().remove(&(100, 201));
    data.publication_status(&http, seller(), item.id())
        .await
        .unwrap();
    assert!(
        mock.posts()
            .iter()
            .any(|body| body.contains("ロール") && body.contains("500"))
    );
    offer(&http, &data, &item, 911).await.unwrap();
    let (first, second) = tokio::join!(confirm(&http, &data, 911), confirm(&http, &data, 911));
    assert!(first.is_ok() || second.is_ok());
    assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert_eq!(
        data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
        700
    );
    assert_eq!(
        data.sqlite_not.get_balance(seller()).await.unwrap().get(),
        0
    );
    let purchase = data
        .sqlite_shop
        .list_purchases(buyer())
        .await
        .unwrap()
        .remove(0)
        .buyer()
        .id();
    assert!(data.deliver_role(&http, actor(), purchase).await.is_err());
    assert!(
        data.refund_role(&http, buyer(), purchase, "unauthorized")
            .await
            .is_err()
    );
    let purchases = data.sqlite_shop.list_purchases(buyer()).await.unwrap();
    assert!(
        Data::purchase_page(&http, buyer(), &purchases, 0)
            .await
            .unwrap()
            .0
            .contains("会員権")
    );
    mock.role_change_mode.store(1, Ordering::Relaxed);
    assert!(
        data.refund_role(&http, seller(), purchase, "manual")
            .await
            .is_err()
    );
    assert_eq!(
        data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
        700
    );
    mock.role_change_mode.store(0, Ordering::Relaxed);
    let original = data
        .refund_role(&http, seller(), purchase, "manual")
        .await
        .unwrap();
    assert_eq!(
        data.refund_role(&http, seller(), purchase, "manual")
            .await
            .unwrap(),
        original
    );
    let message = data
        .refund_purchase(&http, seller(), purchase, "manual")
        .await
        .unwrap();
    assert!(message.contains("500 NOTを返金"));
    assert!(!message.contains("売上取消ID"));
    assert!(!mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert_eq!(
        data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
        1200
    );
    assert!(data.deliver_role(&http, buyer(), purchase).await.is_err());
    let definition = to_value(
        super::super::commands::role()
            .create_as_slash_command()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(definition["name"], "role");
    assert!(
        definition["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["name"] == "recover")
    );
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn 出品者本人のロール購入も代金を減らし返金は元の購入額を戻す() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    data.sqlite_not
        .adjust_balance(
            seller(),
            seller().user_id(),
            domain::Balance::new(i64::MAX).unwrap(),
            "seller-fund",
        )
        .await
        .unwrap();
    let mut click = component(&format!("content:buy:100:{}", item.id()), 999);
    click.guild_id = Some(GuildId::new(100));
    click.id = InteractionId::new(930);
    data.purchase_action(&http, &click, SystemTime::now())
        .await
        .unwrap();
    assert!(mock.posts().iter().any(|body| {
        let payload: serenity::json::Value = serenity::json::from_str(body).unwrap();
        payload["embeds"][0]["fields"]
            .as_array()
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    field["name"] == "購入後の残高"
                        && field["value"] == format!("{} NOT", i64::MAX - 500)
                })
            })
    }));
    let confirm = component("content:confirm:100-930:yes", 999);
    for _ in 0..2 {
        data.purchase_action(&http, &confirm, SystemTime::now())
            .await
            .unwrap();
    }
    let purchases = data.sqlite_shop.list_purchases(seller()).await.unwrap();
    assert_eq!(purchases.len(), 1);
    let purchase = purchases[0].buyer().id();
    assert!(mock.role_members.lock().unwrap().contains(&(100, 999)));
    assert_eq!(
        data.sqlite_not.get_balance(seller()).await.unwrap().get(),
        i64::MAX - 500
    );
    data.refund_role(&http, seller(), purchase, "self-return")
        .await
        .unwrap();
    assert!(!mock.role_members.lock().unwrap().contains(&(100, 999)));
    assert_eq!(
        data.sqlite_not.get_balance(seller()).await.unwrap().get(),
        i64::MAX
    );
    assert_eq!(
        data.sqlite_shop.get_purchase(seller(), purchase).await,
        Err(RepositoryError::AlreadyRefunded)
    );

    let quote = data
        .sqlite_shop
        .prepare_purchase(seller(), item.id(), "self-rebuy", SystemTime::now())
        .await
        .unwrap();
    let purchase = data
        .sqlite_shop
        .purchase(seller(), &quote, SystemTime::now())
        .await
        .unwrap()
        .buyer()
        .id();
    data.sqlite_not
        .adjust_balance(
            seller(),
            seller().user_id(),
            domain::Balance::new(0).unwrap(),
            "seller-spent",
        )
        .await
        .unwrap();
    mock.role_change_mode.store(1, Ordering::Relaxed);
    assert!(
        data.deliver_role(&http, seller(), purchase)
            .await
            .unwrap()
            .0
            .contains("返金しました")
    );
    assert_eq!(
        data.sqlite_not.get_balance(seller()).await.unwrap().get(),
        500
    );
    assert_eq!(
        data.sqlite_shop.get_purchase(seller(), purchase).await,
        Err(RepositoryError::AlreadyRefunded)
    );
    data.database.close().await.unwrap();
}

#[tokio::test]
async fn ロールの未付与を確認できたときだけ返金し結果不明は再起動後も照合できる() {
    for mode in [1, 2, 3] {
        let (directory, data, item) = scenario().await;
        let mock = MockDiscord::new();
        let http = mock.http();
        offer(&http, &data, &item, 920).await.unwrap();
        mock.role_change_mode.store(mode, Ordering::Relaxed);
        let (text, _) = confirm(&http, &data, 920).await.unwrap();
        if mode == 1 {
            assert!(text.contains("返金しました"));
            assert_eq!(
                data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
                1200
            );
            assert_eq!(
                data.sqlite_not.get_balance(seller()).await.unwrap().get(),
                0
            );
            assert!(
                data.sqlite_shop
                    .list_role_purchases(buyer().guild_id())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(confirm(&http, &data, 920).await.is_err());
        } else if mode == 2 {
            assert!(text.contains("付与・確認"));
            assert_eq!(
                data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
                700
            );
            assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
        } else {
            assert!(text.contains("RoleDeliveryPending"));
            assert!(!text.contains("private-role"));
            assert_eq!(
                data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
                700
            );
            data.sqlite_shop
                .delete(seller(), item.id(), 1)
                .await
                .unwrap();
            data.database.close().await.unwrap();
            let data = setup(&directory).await;
            mock.role_change_mode.store(0, Ordering::Relaxed);
            mock.fail_role_lookup.store(false, Ordering::Relaxed);
            data.reconcile_roles(&http, GuildId::new(100))
                .await
                .unwrap();
            assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
            assert_eq!(
                data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
                700
            );
            assert_eq!(
                data.sqlite_shop
                    .list_role_purchases(buyer().guild_id())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            confirm(&http, &data, 920).await.unwrap();
            data.database.close().await.unwrap();
            continue;
        }
        assert!(
            !mock
                .posts()
                .iter()
                .any(|body| body.contains("name=\"files[0]\""))
        );
        data.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn 返金で購入者の残高上限を超える場合は権利を維持し解除したロールを復元する() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let quote = data
        .sqlite_shop
        .prepare_purchase(buyer(), item.id(), "paid-before-crash", SystemTime::now())
        .await
        .unwrap();
    let purchase = data
        .sqlite_shop
        .purchase(buyer(), &quote, SystemTime::now())
        .await
        .unwrap()
        .buyer()
        .id();
    data.sqlite_not
        .adjust_balance(
            seller(),
            buyer().user_id(),
            domain::Balance::new(i64::MAX).unwrap(),
            "buyer-at-limit",
        )
        .await
        .unwrap();
    mock.role_change_mode.store(1, Ordering::Relaxed);
    assert!(
        data.deliver_role(&http, buyer(), purchase)
            .await
            .unwrap()
            .0
            .contains("RoleDeliveryPending")
    );
    assert_eq!(
        data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
        i64::MAX
    );
    assert!(
        data.sqlite_shop
            .get_purchase(buyer(), purchase)
            .await
            .is_ok()
    );
    mock.role_change_mode.store(0, Ordering::Relaxed);
    data.reconcile_roles(&http, GuildId::new(100))
        .await
        .unwrap();
    assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert!(
        data.refund_role(&http, seller(), purchase, "buyer-overflow")
            .await
            .is_err()
    );
    assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert_eq!(
        data.sqlite_not.get_balance(buyer()).await.unwrap().get(),
        i64::MAX
    );
    let foreign = domain::Actor::new(domain::GuildId::new(101).unwrap(), buyer().user_id(), true);
    assert!(data.deliver_role(&http, foreign, purchase).await.is_err());
    assert!(
        data.refund_role(&http, foreign, purchase, "foreign")
            .await
            .is_err()
    );
    assert_eq!(
        data.sqlite_shop
            .list_role_purchases(foreign.guild_id())
            .await
            .unwrap()
            .len(),
        0
    );
    data.database.close().await.unwrap();
}
