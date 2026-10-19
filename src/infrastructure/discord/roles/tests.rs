use std::{sync::atomic::Ordering, time::SystemTime};

use serenity::json::{json, to_value};

use super::*;
use crate::{
    domain::Balance,
    infrastructure::discord::{purchases, test_support::*},
};

fn seller() -> Actor {
    Actor::new(
        actor().guild_id(),
        crate::domain::UserId::try_from(999).unwrap(),
        true,
    )
}
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
    data.not
        .adjust_balance(
            seller(),
            buyer().user_id(),
            Balance::try_from(1200).unwrap(),
            "fund",
        )
        .await
        .unwrap();
    let item = data
        .shop
        .create_role(
            seller(),
            crate::domain::RoleId::try_from(500).unwrap(),
            listings::item_details("会員権".into(), "会員ロール".into(), "500").unwrap(),
        )
        .await
        .unwrap();
    (directory, data, item)
}
async fn offer(http: &Http, data: &Data, item: &ShopItem, id: u64) -> Result<(), Error> {
    let mut click = component(&format!("content:buy:100:{}", item.id()), 201);
    click.guild_id = Some(GuildId::new(100));
    click.id = InteractionId::new(id);
    purchases::handle_action(http, data, &click, SystemTime::now()).await?;
    Ok(())
}
async fn confirm(
    http: &Http,
    data: &Data,
    id: u64,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    purchases::handle_action(
        http,
        data,
        &component(&format!("content:confirm:100-{id}:yes"), 201),
        SystemTime::now(),
    )
    .await
}

#[tokio::test]
async fn role_sales_validate_permissions_use_shared_checkout_and_revoke_before_refund() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    assert!(
        services::authenticate(&http, GuildId::new(100), UserId::new(999))
            .await
            .unwrap()
            .can_manage_not()
    );
    let role = crate::domain::RoleId::try_from(500).unwrap();
    assert!(
        data.shop
            .create_role(actor(), role, item.details().clone())
            .await
            .is_err()
    );
    assert_eq!(
        data.shop
            .create_role(seller(), role, item.details().clone())
            .await
            .unwrap(),
        item
    );
    assert!(
        data.shop
            .prepare_purchase(seller(), item.id(), "self", SystemTime::now())
            .await
            .is_err()
    );
    for (index, field, value) in [
        (0, "managed", json!(true)),
        (0, "position", json!(3)),
        (1, "permissions", json!("0")),
    ] {
        let old = mock.role_definitions.lock().unwrap()[index][field].clone();
        mock.role_definitions.lock().unwrap()[index][field] = value;
        assert!(offer(&http, &data, &item, 910).await.is_err());
        assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
        mock.role_definitions.lock().unwrap()[index][field] = old;
    }
    assert!(
        check_role(&http, GuildId::new(100), RoleId::new(100))
            .await
            .is_err()
    );
    mock.role_members.lock().unwrap().insert((100, 201));
    assert!(offer(&http, &data, &item, 910).await.is_err());
    mock.role_members.lock().unwrap().remove(&(100, 201));
    listings::publication_status(&http, &data, seller(), item.id())
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
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    assert_eq!(data.not.get_balance(seller()).await.unwrap().get(), 500);
    let purchase = data
        .shop
        .list_purchases(buyer())
        .await
        .unwrap()
        .remove(0)
        .buyer()
        .id();
    assert!(deliver(&http, &data, actor(), purchase).await.is_err());
    assert!(
        refund(&http, &data, buyer(), purchase, "unauthorized")
            .await
            .is_err()
    );
    assert!(
        purchases::list(&data, buyer(), 0)
            .await
            .unwrap()
            .0
            .contains("会員権")
    );
    mock.role_change_mode.store(1, Ordering::Relaxed);
    assert!(
        refund(&http, &data, seller(), purchase, "manual")
            .await
            .is_err()
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    mock.role_change_mode.store(0, Ordering::Relaxed);
    let original = refund(&http, &data, seller(), purchase, "manual")
        .await
        .unwrap();
    assert_eq!(
        refund(&http, &data, seller(), purchase, "manual")
            .await
            .unwrap(),
        original
    );
    assert!(!mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
    assert!(deliver(&http, &data, buyer(), purchase).await.is_err());
    let definition = to_value(super::role().create_as_slash_command().unwrap()).unwrap();
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
async fn role_failure_refunds_only_confirmed_absence_and_unknown_state_recovers_after_restart() {
    for mode in [1, 2, 3] {
        let (directory, data, item) = scenario().await;
        let mock = MockDiscord::new();
        let http = mock.http();
        offer(&http, &data, &item, 920).await.unwrap();
        mock.role_change_mode.store(mode, Ordering::Relaxed);
        let (text, _) = confirm(&http, &data, 920).await.unwrap();
        if mode == 1 {
            assert!(text.contains("返金しました"));
            assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 1200);
            assert_eq!(data.not.get_balance(seller()).await.unwrap().get(), 0);
            assert!(
                data.shop
                    .list_role_purchases(buyer().guild_id())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(confirm(&http, &data, 920).await.is_err());
        } else if mode == 2 {
            assert!(text.contains("付与・確認"));
            assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
            assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
        } else {
            assert!(text.contains("RoleDeliveryPending"));
            assert!(!text.contains("private-role"));
            assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
            data.shop.delete(seller(), item.id(), 1).await.unwrap();
            data.database.close().await.unwrap();
            let data = setup(&directory).await;
            mock.role_change_mode.store(0, Ordering::Relaxed);
            mock.fail_role_lookup.store(false, Ordering::Relaxed);
            reconcile(&http, &data, GuildId::new(100)).await.unwrap();
            assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
            assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
            assert_eq!(
                data.shop
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
async fn role_delivery_survives_seller_shortfall_and_failed_manual_refund_restores_entitlement() {
    let (_directory, data, item) = scenario().await;
    let mock = MockDiscord::new();
    let http = mock.http();
    let quote = data
        .shop
        .prepare_purchase(buyer(), item.id(), "paid-before-crash", SystemTime::now())
        .await
        .unwrap();
    let purchase = data
        .shop
        .purchase(buyer(), &quote, SystemTime::now())
        .await
        .unwrap()
        .buyer()
        .id();
    data.not
        .adjust_balance(
            seller(),
            seller().user_id(),
            Balance::try_from(0).unwrap(),
            "spent-sale",
        )
        .await
        .unwrap();
    mock.role_change_mode.store(1, Ordering::Relaxed);
    assert!(
        deliver(&http, &data, buyer(), purchase)
            .await
            .unwrap()
            .0
            .contains("RoleDeliveryPending")
    );
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    assert!(data.shop.get_purchase(buyer(), purchase).await.is_ok());
    mock.role_change_mode.store(0, Ordering::Relaxed);
    reconcile(&http, &data, GuildId::new(100)).await.unwrap();
    assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert!(
        refund(&http, &data, seller(), purchase, "insufficient-sale")
            .await
            .is_err()
    );
    assert!(mock.role_members.lock().unwrap().contains(&(100, 201)));
    assert_eq!(data.not.get_balance(buyer()).await.unwrap().get(), 700);
    let foreign = Actor::new(
        crate::domain::GuildId::try_from(101).unwrap(),
        buyer().user_id(),
        true,
    );
    assert!(deliver(&http, &data, foreign, purchase).await.is_err());
    assert!(
        refund(&http, &data, foreign, purchase, "foreign")
            .await
            .is_err()
    );
    assert_eq!(
        data.shop
            .list_role_purchases(foreign.guild_id())
            .await
            .unwrap()
            .len(),
        0
    );
    data.database.close().await.unwrap();
}
