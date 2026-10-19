use notbot::domain::{
    Actor, Balance, ChannelId, ContentFile, GuildId, ITEM_DESCRIPTION_MAX_CHARS,
    ITEM_NAME_MAX_CHARS, ItemDetails, ItemId, ItemTarget, LedgerEntry, LedgerEntryId, LedgerReason,
    MoneyError, NewShopItem, NotAmount, Price, RoleId, ShopItem, UserId, ValidationError,
};

#[test]
fn money_rejects_invalid_values_and_overdrafts() {
    for value in [i64::MIN, -1, 0] {
        assert_eq!(Price::try_from(value), Err(MoneyError::InvalidPrice));
    }
    assert_eq!(Price::try_from(i64::MAX).unwrap().get(), i64::MAX);
    assert_eq!(Balance::try_from(-1), Err(MoneyError::NegativeBalance));
    let balance = Balance::try_from(100).unwrap();
    assert_eq!(balance.apply(NotAmount::new(-100)).unwrap().get(), 0);
    assert_eq!(
        balance.apply(NotAmount::new(-101)),
        Err(MoneyError::NegativeBalance)
    );
    assert_eq!(balance.apply(NotAmount::new(50)).unwrap().get(), 150);
    assert_eq!(NotAmount::new(0).get(), 0);
    assert_eq!(NotAmount::new(i64::MIN).get(), i64::MIN);
}

#[test]
fn money_checks_addition_subtraction_and_negation_boundaries() {
    let max = NotAmount::new(i64::MAX);
    let min = NotAmount::new(i64::MIN);
    assert_eq!(
        max.checked_add(NotAmount::new(1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        min.checked_add(NotAmount::new(-1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        max.checked_sub(NotAmount::new(-1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(
        min.checked_sub(NotAmount::new(1)),
        Err(MoneyError::Overflow)
    );
    assert_eq!(min.negated(), Err(MoneyError::Overflow));
    assert_eq!(max.negated().unwrap().get(), -i64::MAX);
    assert_eq!(
        NotAmount::new(300).checked_add(NotAmount::new(200)),
        Ok(NotAmount::new(500))
    );
    assert_eq!(
        NotAmount::new(500).checked_sub(NotAmount::new(200)),
        Ok(NotAmount::new(300))
    );
    assert_eq!(
        Balance::try_from(i64::MAX)
            .unwrap()
            .apply(NotAmount::new(1)),
        Err(MoneyError::Overflow)
    );
}

#[test]
fn ids_validate_numeric_ranges_and_canonicalize_decimal_strings() {
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
        assert!(invalid.parse::<GuildId>().is_err(), "{invalid}");
    }
    assert_eq!("001".parse::<GuildId>().unwrap().to_string(), "1");
    assert_eq!(UserId::try_from(u64::MAX).unwrap().get(), u64::MAX);
    assert!(ChannelId::try_from(0).is_err());
    assert!(RoleId::try_from(0).is_err());
    assert!(ItemId::try_from(-1).is_err());
    assert!(LedgerEntryId::try_from(0).is_err());
    assert!("9223372036854775808".parse::<LedgerEntryId>().is_err());
    assert_eq!(ItemId::try_from(i64::MAX).unwrap().get(), i64::MAX);
}

#[test]
fn item_text_uses_character_limits_and_preserves_description_formatting() {
    let price = Price::try_from(500).unwrap();
    let details = ItemDetails::try_new(
        "あ".repeat(ITEM_NAME_MAX_CHARS),
        "🦀".repeat(ITEM_DESCRIPTION_MAX_CHARS),
        price,
    )
    .unwrap();
    assert_eq!(details.name().chars().count(), ITEM_NAME_MAX_CHARS);
    assert_eq!(
        ItemDetails::try_new("あ".repeat(ITEM_NAME_MAX_CHARS + 1), "", price),
        Err(ValidationError::InvalidItemName)
    );
    assert_eq!(
        ItemDetails::try_new("商品", "あ".repeat(ITEM_DESCRIPTION_MAX_CHARS + 1), price),
        Err(ValidationError::InvalidDescription)
    );
    for name in ["", " \t\n", "商品\0", "商品\n名"] {
        assert_eq!(
            ItemDetails::try_new(name, "", price),
            Err(ValidationError::InvalidItemName)
        );
    }
    assert_eq!(
        ItemDetails::try_new("商品", "説明\0", price),
        Err(ValidationError::InvalidDescription)
    );
    let details = ItemDetails::try_new(" 商品 ", "1行目\n2行目\t補足", price).unwrap();
    assert_eq!(details.name(), "商品");
    assert_eq!(details.description(), "1行目\n2行目\t補足");
    assert_eq!(details.price(), price);
}

#[test]
fn content_metadata_rejects_unsafe_keys_and_unrepresentable_sizes() {
    let file = ContentFile::try_new("content_123-abc", "資料集.pdf", 1024).unwrap();
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
            ContentFile::try_new(key, "file", 1),
            Err(ValidationError::InvalidFileKey)
        );
    }
    assert_eq!(
        ContentFile::try_new("a".repeat(256), "file", 1),
        Err(ValidationError::InvalidFileKey)
    );
    for filename in ["", " ", ".", "..", "../file", r"a\b", "file\nname"] {
        assert_eq!(
            ContentFile::try_new("key", filename, 1),
            Err(ValidationError::InvalidFileName)
        );
    }
    for size in [0, i64::MAX as u64 + 1] {
        assert_eq!(
            ContentFile::try_new("key", "file", size),
            Err(ValidationError::InvalidFileSize)
        );
    }
}

#[test]
fn actor_and_product_models_keep_identity_and_revision_separate_from_details() {
    let actor = Actor::new(
        GuildId::try_from(100).unwrap(),
        UserId::try_from(200).unwrap(),
        false,
    );
    assert!(!actor.can_manage_not());
    let details = ItemDetails::try_new("資料集", "", Price::try_from(500).unwrap()).unwrap();
    let target = ItemTarget::Content(ContentFile::try_new("file_1", "資料.pdf", 100).unwrap());
    let new_item = NewShopItem::new(details.clone(), target.clone());
    let item_id = ItemId::try_from(1).unwrap();
    let item = ShopItem::try_new(
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
            ShopItem::try_new(
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
    let deleted = ShopItem::try_new(
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
fn ledger_reason_codes_and_sign_rules_match_the_fixed_master_contract() {
    let source = LedgerEntryId::try_from(2).unwrap();
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
            LedgerReason::Purchase(ItemId::try_from(1).unwrap()),
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
        assert_eq!(reason.code(), code);
        for (amount, allowed) in [-1, 0, 1].into_iter().zip(allowed) {
            let result = LedgerEntry::try_new(
                LedgerEntryId::try_from(1).unwrap(),
                GuildId::try_from(100).unwrap(),
                UserId::try_from(200).unwrap(),
                NotAmount::new(amount),
                reason,
                "test:event",
            );
            assert_eq!(result.is_ok(), allowed, "{code} {amount}");
        }
    }
}

#[test]
fn ledger_entries_require_a_key_and_cannot_reference_themselves() {
    let id = LedgerEntryId::try_from(1).unwrap();
    let guild = GuildId::try_from(100).unwrap();
    let user = UserId::try_from(200).unwrap();
    for key in ["", " \t", "event\n"] {
        assert_eq!(
            LedgerEntry::try_new(
                id,
                guild,
                user,
                NotAmount::new(0),
                LedgerReason::Adjustment,
                key
            ),
            Err(ValidationError::InvalidEventKey)
        );
    }
    assert_eq!(
        LedgerEntry::try_new(
            id,
            guild,
            user,
            NotAmount::new(500),
            LedgerReason::Refund(id),
            "refund:request:buyer",
        ),
        Err(ValidationError::InvalidLedgerReference)
    );
    let reason = LedgerReason::Purchase(ItemId::try_from(2).unwrap());
    let entry = LedgerEntry::try_new(
        id,
        guild,
        user,
        NotAmount::new(-500),
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
