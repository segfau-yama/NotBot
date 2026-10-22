use crate::domain::{ItemTarget, LedgerReason};
use crate::infrastructure::sqlite::shop::ShopRepository as SqliteShopRepository;
use crate::infrastructure::types as values;
use crate::infrastructure::{
    ITEM_DESCRIPTION_MAX_CHARS, ITEM_NAME_MAX_CHARS, MoneyError, ValidationError,
};

#[test]
fn 同じクレートの別モジュールから金額や権限や購入確認を直接書き換えられない() {
    let directory =
        std::env::temp_dir().join(format!("notbot-domain-privacy-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let domain = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain.rs");
    let source = directory.join("privacy.rs");
    std::fs::write(
        &source,
        format!(
            r#"#![allow(dead_code)]
#[path = {domain:?}]
mod domain;
mod commands {{
    use crate::domain::{{Actor, Price, PurchaseQuote}};
    pub fn overwrite(price: &mut Price, actor: &mut Actor, quote: &mut PurchaseQuote) {{
        price.0 = -1;
        actor.can_manage_not = true;
        quote.expires_at = std::time::UNIX_EPOCH;
    }}
}}
"#
        ),
    )
    .unwrap();
    let output =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args([
                "--crate-name=domain_privacy",
                "--edition=2024",
                "--crate-type=lib",
                "--emit=metadata",
            ])
            .arg(&source)
            .arg("-o")
            .arg(directory.join("privacy.rmeta"))
            .output()
            .unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(
        !output.status.success(),
        "domainのフィールドを直接変更できてしまった"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("E0616"),
        "可視性以外の理由で失敗した: {error}"
    );
    for field in ["0", "can_manage_not", "expires_at"] {
        assert!(
            error.contains(&format!("field `{field}`")),
            "{field}の非公開化を確認できない: {error}"
        );
    }
}

#[test]
fn 価格は正の整数だけを受け付ける() {
    for value in [i64::MIN, -1, 0] {
        assert_eq!(values::price(value), Err(MoneyError::InvalidPrice));
    }
    assert_eq!(values::price(i64::MAX).unwrap().get(), i64::MAX);
}

#[test]
fn 残高はゼロ以上の整数だけを受け付ける() {
    assert_eq!(values::balance(-1), Err(MoneyError::NegativeBalance));
    assert_eq!(values::balance(0).unwrap().get(), 0);
}

#[test]
fn 残高を超える減算は拒否する() {
    let balance = values::balance(100).unwrap();

    assert_eq!(
        values::apply_balance(balance, values::amount(-101)),
        Err(MoneyError::NegativeBalance)
    );
}

#[test]
fn 残高に増減額を反映できる() {
    let balance = values::balance(100).unwrap();

    assert_eq!(
        values::apply_balance(balance, values::amount(-100))
            .unwrap()
            .get(),
        0
    );
    assert_eq!(
        values::apply_balance(balance, values::amount(50))
            .unwrap()
            .get(),
        150
    );
}

#[test]
fn 増減額にはゼロと負数も使える() {
    assert_eq!(values::amount(0).get(), 0);
    assert_eq!(values::amount(i64::MIN).get(), i64::MIN);
}

#[test]
fn 増減額の加減算が整数範囲を超えたら拒否する() {
    let max = values::amount(i64::MAX);
    let min = values::amount(i64::MIN);

    assert_eq!(
        values::checked_add(max, values::amount(1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        values::checked_add(min, values::amount(-1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        values::checked_sub(max, values::amount(-1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        values::checked_sub(min, values::amount(1)),
        Err(MoneyError::Overflow)
    );
}

#[test]
fn 符号反転が整数範囲を超えたら拒否する() {
    assert_eq!(
        values::negated(values::amount(i64::MIN)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        values::negated(values::amount(i64::MAX)).unwrap().get(),
        -i64::MAX
    );
}

#[test]
fn 増減額の加減算を計算できる() {
    assert_eq!(
        values::checked_add(values::amount(300), values::amount(200)),
        Ok(values::amount(500))
    );
    assert_eq!(
        values::checked_sub(values::amount(500), values::amount(200)),
        Ok(values::amount(300))
    );
}

#[test]
fn 残高の加算が整数上限を超えたら拒否する() {
    let balance = values::balance(i64::MAX).unwrap();

    assert_eq!(
        values::apply_balance(balance, values::amount(1)),
        Err(MoneyError::Overflow)
    );
}

#[test]
fn 識別子は正の十進整数に限り先頭のゼロを正規化する() {
    for invalid in [
        "",
        "0",
        "-1",
        "+1",
        " 1",
        "1 ",
        "１２",
        "18446744073709551616",
    ] {
        assert!(values::parse_guild_id(invalid).is_err(), "{invalid}");
    }
    assert_eq!(values::parse_guild_id("001").unwrap().to_string(), "1");
    assert_eq!(values::user_id(u64::MAX).unwrap().get(), u64::MAX);
    assert!(values::channel_id(0).is_err());
    assert!(values::role_id(0).is_err());
    assert!(values::item_id(-1).is_err());
    assert!(values::ledger_entry_id(0).is_err());
    assert!(values::parse_ledger_entry_id("9223372036854775808").is_err());
    assert_eq!(values::item_id(i64::MAX).unwrap().get(), i64::MAX);
}

#[test]
fn 商品名と説明は文字数上限を守り説明の改行とタブを保つ() {
    let price = values::price(500).unwrap();
    let details = values::item_details(
        "あ".repeat(ITEM_NAME_MAX_CHARS),
        "🦀".repeat(ITEM_DESCRIPTION_MAX_CHARS),
        price,
    )
    .unwrap();
    assert_eq!(details.name().chars().count(), ITEM_NAME_MAX_CHARS);
    assert_eq!(
        values::item_details("あ".repeat(ITEM_NAME_MAX_CHARS + 1), "", price),
        Err(ValidationError::InvalidItemName)
    );
    assert_eq!(
        values::item_details("商品", "あ".repeat(ITEM_DESCRIPTION_MAX_CHARS + 1), price),
        Err(ValidationError::InvalidDescription)
    );
    for name in ["", " \t\n", "商品\0", "商品\n名"] {
        assert_eq!(
            values::item_details(name, "", price),
            Err(ValidationError::InvalidItemName)
        );
    }
    assert_eq!(
        values::item_details("商品", "説明\0", price),
        Err(ValidationError::InvalidDescription)
    );
    let details = values::item_details(" 商品 ", "1行目\n2行目\t補足", price).unwrap();
    assert_eq!(details.name(), "商品");
    assert_eq!(details.description(), "1行目\n2行目\t補足");
    assert_eq!(details.price(), price);
}

#[test]
fn コンテンツ情報には安全な保存キーとファイル名と正のサイズだけを許可する() {
    let file = values::content_file("content_123-abc", "資料集.pdf", 1024).unwrap();
    assert_eq!(file.key(), "content_123-abc");
    assert_eq!(file.filename(), "資料集.pdf");
    assert_eq!(file.size_bytes(), 1024);
    for key in [
        "",
        ".",
        "..",
        "../secret",
        "/tmp/file",
        r"a\b",
        "key:1",
        "a\0b",
    ] {
        assert_eq!(
            values::content_file(key, "file", 1),
            Err(ValidationError::InvalidFileKey)
        );
    }
    assert_eq!(
        values::content_file("a".repeat(256), "file", 1),
        Err(ValidationError::InvalidFileKey)
    );
    for filename in ["", " ", ".", "..", "../file", r"a\b", "file\nname"] {
        assert_eq!(
            values::content_file("key", filename, 1),
            Err(ValidationError::InvalidFileName)
        );
    }
    for size in [0, i64::MAX as u64 + 1] {
        assert_eq!(
            values::content_file("key", "file", size),
            Err(ValidationError::InvalidFileSize)
        );
    }
}

#[test]
fn 商品は識別情報と表示内容と版と販売状態を分けて保持する() {
    let actor = values::actor(
        values::guild_id(100).unwrap(),
        values::user_id(200).unwrap(),
        false,
    );
    assert!(!actor.can_manage_not());
    let details = values::item_details("資料集", "", values::price(500).unwrap()).unwrap();
    let target = ItemTarget::Content(values::content_file("file_1", "資料.pdf", 100).unwrap());
    let new_item = values::new_item(details.clone(), target.clone());
    let item_id = values::item_id(1).unwrap();
    let item = values::shop_item(
        item_id,
        actor.guild_id(),
        actor.user_id(),
        new_item.target().clone(),
        new_item.details().clone(),
        1,
        false,
    )
    .unwrap();
    assert_eq!(item.id(), item_id);
    assert_eq!(item.guild_id(), actor.guild_id());
    assert_eq!(item.seller_id(), actor.user_id());
    assert_eq!(item.target(), &target);
    assert_eq!(item.details(), &details);
    assert_eq!(item.revision(), 1);
    assert!(!item.is_deleted());
    for revision in [0, -1] {
        assert_eq!(
            values::shop_item(
                item_id,
                actor.guild_id(),
                actor.user_id(),
                target.clone(),
                details.clone(),
                revision,
                false,
            ),
            Err(ValidationError::InvalidRevision)
        );
    }
    let deleted = values::shop_item(
        item_id,
        actor.guild_id(),
        actor.user_id(),
        target,
        details,
        2,
        true,
    )
    .unwrap();
    assert!(deleted.is_deleted());
    assert_eq!(deleted.details(), item.details());
}

#[test]
fn 取引理由ごとに許可された符号の増減額だけを記録できる() {
    let source = values::ledger_entry_id(2).unwrap();
    let cases = [
        (LedgerReason::Message, "MESSAGE", [false, false, true]),
        (LedgerReason::Reaction, "REACTION", [false, false, true]),
        (
            LedgerReason::ReactionReceived,
            "REACTION_RECEIVED",
            [false, false, true],
        ),
        (
            LedgerReason::ReplyReceived,
            "REPLY_RECEIVED",
            [false, false, true],
        ),
        (LedgerReason::TipSent, "TIP_SENT", [true, false, false]),
        (
            LedgerReason::TipReceived,
            "TIP_RECEIVED",
            [false, false, true],
        ),
        (LedgerReason::Timeout, "TIMEOUT", [true, true, false]),
        (
            LedgerReason::Purchase(values::item_id(1).unwrap()),
            "PURCHASE",
            [true, false, false],
        ),
        (LedgerReason::Sale(source), "SALE", [false, false, true]),
        (LedgerReason::Refund(source), "REFUND", [false, false, true]),
        (
            LedgerReason::SaleRefund(source),
            "SALE_REFUND",
            [true, false, false],
        ),
        (LedgerReason::Adjustment, "ADJUSTMENT", [true, true, true]),
    ];
    for (reason, code, allowed) in cases {
        assert_eq!(values::reason_code(reason), code);
        for (amount, allowed) in [-1, 0, 1].into_iter().zip(allowed) {
            let result = values::ledger_entry(
                values::ledger_entry_id(1).unwrap(),
                values::guild_id(100).unwrap(),
                values::user_id(200).unwrap(),
                values::amount(amount),
                reason,
                "test:event",
            );
            assert_eq!(result.is_ok(), allowed, "{code} {amount}");
        }
    }
}

#[test]
fn 取引には依頼を識別するキーが必要で自分自身を取消元に指定できない() {
    let id = values::ledger_entry_id(1).unwrap();
    let guild = values::guild_id(100).unwrap();
    let user = values::user_id(200).unwrap();
    for key in ["", " \t", "event\n"] {
        assert_eq!(
            values::ledger_entry(
                id,
                guild,
                user,
                values::amount(0),
                LedgerReason::Adjustment,
                key
            ),
            Err(ValidationError::InvalidEventKey)
        );
    }
    assert_eq!(
        values::ledger_entry(
            id,
            guild,
            user,
            values::amount(500),
            LedgerReason::Refund(id),
            "refund:request:buyer",
        ),
        Err(ValidationError::InvalidLedgerReference)
    );
    let reason = LedgerReason::Purchase(values::item_id(2).unwrap());
    let entry = values::ledger_entry(
        id,
        guild,
        user,
        values::amount(-500),
        reason,
        "content:confirmation:buyer",
    )
    .unwrap();
    assert_eq!(entry.id(), id);
    assert_eq!(entry.guild_id(), guild);
    assert_eq!(entry.user_id(), user);
    assert_eq!(entry.amount().get(), -500);
    assert_eq!(entry.reason(), reason);
    assert_eq!(entry.event_key(), "content:confirmation:buyer");
}

#[test]
fn 購入結果は商品のギルドや購入元や売上額が一致しなければ生成しない() {
    use crate::infrastructure::sqlite::{DatabaseError, shop::ShopRepository};
    let guild = values::guild_id(100).unwrap();
    let seller_id = values::user_id(200).unwrap();
    let buyer_id = values::user_id(201).unwrap();
    let item_id = values::item_id(1).unwrap();
    let item = values::shop_item(
        item_id,
        guild,
        seller_id,
        ItemTarget::Content(values::content_file("file", "file.pdf", 1).unwrap()),
        values::item_details("商品", "", values::price(500).unwrap()).unwrap(),
        1,
        false,
    )
    .unwrap();
    let buyer = values::ledger_entry(
        values::ledger_entry_id(1).unwrap(),
        guild,
        buyer_id,
        values::amount(-500),
        LedgerReason::Purchase(item_id),
        "purchase",
    )
    .unwrap();
    for (entry_guild, user, reason, amount) in [
        (guild, seller_id, LedgerReason::Sale(buyer.id()), 499),
        (
            values::guild_id(101).unwrap(),
            seller_id,
            LedgerReason::Sale(buyer.id()),
            500,
        ),
        (guild, buyer_id, LedgerReason::Sale(buyer.id()), 500),
        (
            guild,
            seller_id,
            LedgerReason::Sale(values::ledger_entry_id(3).unwrap()),
            500,
        ),
    ] {
        let seller = values::ledger_entry(
            values::ledger_entry_id(2).unwrap(),
            entry_guild,
            user,
            values::amount(amount),
            reason,
            "sale",
        )
        .unwrap();
        assert_eq!(
            ShopRepository::purchase_result(item.clone(), buyer.clone(), Some(seller)),
            Err(DatabaseError::InvalidData)
        );
    }
    assert_eq!(
        ShopRepository::purchase_result(item.clone(), buyer.clone(), None),
        Err(DatabaseError::InvalidData)
    );
    let wrong_buyer = values::ledger_entry(
        buyer.id(),
        values::guild_id(101).unwrap(),
        buyer_id,
        buyer.amount(),
        buyer.reason(),
        "purchase",
    )
    .unwrap();
    assert_eq!(
        ShopRepository::purchase_result(item, wrong_buyer, None),
        Err(DatabaseError::InvalidData)
    );
}

#[test]
fn 購入結果は現在価格が変わっていても元の購入額と売上を照合する() {
    use crate::infrastructure::sqlite::DatabaseError;
    let guild = values::guild_id(100).unwrap();
    let seller_id = values::user_id(200).unwrap();
    let buyer_id = values::user_id(201).unwrap();
    let id = values::item_id(1).unwrap();
    let item = values::shop_item(
        id,
        guild,
        seller_id,
        ItemTarget::Content(values::content_file("file", "file.pdf", 1).unwrap()),
        values::item_details("商品", "", values::price(700).unwrap()).unwrap(),
        2,
        false,
    )
    .unwrap();
    let buyer = values::ledger_entry(
        values::ledger_entry_id(1).unwrap(),
        guild,
        buyer_id,
        values::amount(-500),
        LedgerReason::Purchase(id),
        "purchase",
    )
    .unwrap();
    let seller = values::ledger_entry(
        values::ledger_entry_id(2).unwrap(),
        guild,
        seller_id,
        values::amount(500),
        LedgerReason::Sale(buyer.id()),
        "sale",
    )
    .unwrap();
    let purchase =
        SqliteShopRepository::purchase_result(item.clone(), buyer.clone(), Some(seller.clone()))
            .unwrap();
    assert_eq!(purchase.buyer().amount().get(), -500);
    assert_eq!(purchase.item().details().price().get(), 700);
    for reason in [
        LedgerReason::Purchase(values::item_id(2).unwrap()),
        LedgerReason::TipSent,
    ] {
        let other =
            values::ledger_entry(buyer.id(), guild, buyer_id, buyer.amount(), reason, "other")
                .unwrap();
        assert_eq!(
            SqliteShopRepository::purchase_result(item.clone(), other, Some(seller.clone())),
            Err(DatabaseError::InvalidData)
        );
    }
    let other = values::ledger_entry(
        buyer.id(),
        values::guild_id(101).unwrap(),
        buyer_id,
        buyer.amount(),
        buyer.reason(),
        "other-guild",
    )
    .unwrap();
    assert_eq!(
        SqliteShopRepository::purchase_result(item, other, Some(seller)),
        Err(DatabaseError::InvalidData)
    );
}

#[test]
fn 返金結果は元の購入と売上の本人や参照や金額が一致する場合だけ生成する() {
    use crate::infrastructure::sqlite::DatabaseError;
    let guild = values::guild_id(100).unwrap();
    let seller_id = values::user_id(200).unwrap();
    let buyer_id = values::user_id(201).unwrap();
    let item_id = values::item_id(1).unwrap();
    let item = values::shop_item(
        item_id,
        guild,
        seller_id,
        ItemTarget::Content(values::content_file("file", "file.pdf", 1).unwrap()),
        values::item_details("商品", "", values::price(500).unwrap()).unwrap(),
        1,
        false,
    )
    .unwrap();
    let buy = values::ledger_entry(
        values::ledger_entry_id(1).unwrap(),
        guild,
        buyer_id,
        values::amount(-500),
        LedgerReason::Purchase(item_id),
        "buy",
    )
    .unwrap();
    let sale = values::ledger_entry(
        values::ledger_entry_id(2).unwrap(),
        guild,
        seller_id,
        values::amount(500),
        LedgerReason::Sale(buy.id()),
        "sale",
    )
    .unwrap();
    let purchase =
        SqliteShopRepository::purchase_result(item, buy.clone(), Some(sale.clone())).unwrap();
    let refund = values::ledger_entry(
        values::ledger_entry_id(3).unwrap(),
        guild,
        buyer_id,
        values::amount(500),
        LedgerReason::Refund(buy.id()),
        "refund",
    )
    .unwrap();
    let cancellation = values::ledger_entry(
        values::ledger_entry_id(4).unwrap(),
        guild,
        seller_id,
        values::amount(-500),
        LedgerReason::SaleRefund(sale.id()),
        "cancel-sale",
    )
    .unwrap();
    assert!(
        SqliteShopRepository::refund_result(&purchase, refund.clone(), Some(cancellation.clone()))
            .is_ok()
    );
    assert_eq!(
        SqliteShopRepository::refund_result(&purchase, refund.clone(), None),
        Err(DatabaseError::InvalidData)
    );
    for (entry_guild, user, reason, amount) in [
        (guild, buyer_id, LedgerReason::Refund(buy.id()), 499),
        (guild, seller_id, LedgerReason::Refund(buy.id()), 500),
        (
            values::guild_id(101).unwrap(),
            buyer_id,
            LedgerReason::Refund(buy.id()),
            500,
        ),
        (guild, buyer_id, LedgerReason::Refund(sale.id()), 500),
    ] {
        let wrong = values::ledger_entry(
            refund.id(),
            entry_guild,
            user,
            values::amount(amount),
            reason,
            "wrong-refund",
        )
        .unwrap();
        assert_eq!(
            SqliteShopRepository::refund_result(&purchase, wrong, Some(cancellation.clone())),
            Err(DatabaseError::InvalidData)
        );
    }
    let wrong = values::ledger_entry(
        cancellation.id(),
        guild,
        seller_id,
        values::amount(-499),
        cancellation.reason(),
        "wrong-cancellation",
    )
    .unwrap();
    assert_eq!(
        SqliteShopRepository::refund_result(&purchase, refund, Some(wrong)),
        Err(DatabaseError::InvalidData)
    );
}
