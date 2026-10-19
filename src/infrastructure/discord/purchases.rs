use std::time::SystemTime;

use serenity::all::*;

use super::{Data, content, roles, services};
use crate::{
    domain::{Actor, ItemId, ItemTarget, LedgerEntryId, NotAmount},
    infrastructure::{Error, UseCaseError},
};

fn resend_button(actor: Actor, purchase: LedgerEntryId) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("content:resend:{}:{purchase}", actor.guild_id()))
            .label(format!("無料再送 #{purchase}"))
            .style(ButtonStyle::Primary),
    ])
}

async fn prepare(
    http: &Http,
    data: &Data,
    actor: Actor,
    item: ItemId,
    id: String,
    now: SystemTime,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let quote = match data.shop.prepare_purchase(actor, item, &id, now).await {
        Err(UseCaseError::AlreadyPurchased) => {
            let purchase = data
                .shop
                .list_purchases(actor)
                .await?
                .into_iter()
                .find(|purchase| purchase.item().id() == item)
                .ok_or(UseCaseError::PurchaseNotFound)?;
            return Ok((
                "購入済みです。追加課金なしで再送できます。".into(),
                vec![resend_button(actor, purchase.buyer().id())],
            ));
        }
        result => result?,
    };
    if matches!(quote.item().target(), ItemTarget::Role(_)) {
        roles::preflight(http, actor, quote.item()).await?;
    }
    let balance = data.not.get_balance(actor).await?;
    let price = quote.item().details().price().get();
    let after = if quote.item().seller_id() == actor.user_id() {
        balance
    } else {
        balance.apply(NotAmount::new(-price))?
    };
    let guild = http.get_guild(GuildId::new(actor.guild_id().get())).await?;
    let message = CreateMessage::new()
        .embed(
            CreateEmbed::new()
                .title("購入の確認")
                .description(quote.item().details().name())
                .field("サーバー", guild.name, false)
                .field("出品者", format!("<@{}>", quote.item().seller_id()), false)
                .field(
                    "お届け内容",
                    match quote.item().target() {
                        ItemTarget::Content(file) => file.filename().to_owned(),
                        ItemTarget::Role(role) => format!("ロール <@&{role}>"),
                    },
                    false,
                )
                .field("価格", format!("{price} NOT"), true)
                .field("現在の残高", format!("{} NOT", balance.get()), true)
                .field("購入後の残高", format!("{} NOT", after.get()), true)
                .field("確認期限", "発行から5分", false),
        )
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("content:confirm:{id}:yes"))
                .label(format!("はい（{price} NOTで購入）"))
                .style(ButtonStyle::Success),
            CreateButton::new(format!("content:confirm:{id}:no"))
                .label("いいえ")
                .style(ButtonStyle::Secondary),
        ])])
        .allowed_mentions(CreateAllowedMentions::new());
    {
        let mut confirmations = data.confirmations.lock().await;
        confirmations.retain(|_, quote| now < quote.expires_at());
        confirmations.insert(id.clone(), quote);
    }
    if UserId::new(actor.user_id().get())
        .dm(http, message)
        .await
        .is_err()
    {
        data.confirmations.lock().await.remove(&id);
        return Err(
            "購入確認のDMを送信できませんでした。課金していません。DM設定を確認してください。"
                .into(),
        );
    }
    println!(
        "content confirmation guild={} item={} confirmation={id}",
        actor.guild_id(),
        item
    );
    Ok((
        "DMへ購入確認を送りました。表示された価格を確認してください。".into(),
        vec![],
    ))
}

async fn deliver(
    http: &Http,
    data: &Data,
    actor: Actor,
    purchase: LedgerEntryId,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    if let Ok(receipt) = data.shop.get_purchase(actor, purchase).await
        && matches!(receipt.item().target(), ItemTarget::Role(_))
    {
        return roles::deliver(http, data, actor, purchase).await;
    }
    let result: Result<(), Error> = async {
        let file = data.shop.get_content_for_purchase(actor, purchase).await?;
        let bytes = content::read_content(&data.content_dir, &file).await?;
        let dm = UserId::new(actor.user_id().get())
            .create_dm_channel(http)
            .await?;
        data.shop.get_content_for_purchase(actor, purchase).await?;
        dm.send_message(
            http,
            CreateMessage::new()
                .content(format!(
                    "購入ID: {purchase}（再送を含め追加課金はありません）"
                ))
                .add_file(CreateAttachment::bytes(bytes, file.filename()))
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            println!(
                "content delivery guild={} purchase={purchase}",
                actor.guild_id()
            );
            Ok((
                format!("ファイルをDMへ送信しました。購入ID: {purchase}"),
                vec![],
            ))
        }
        Err(error)
            if matches!(
                error.downcast_ref::<UseCaseError>(),
                Some(
                    UseCaseError::PermissionDenied
                        | UseCaseError::PurchaseNotFound
                        | UseCaseError::AlreadyRefunded
                )
            ) =>
        {
            Err(error)
        }
        Err(_) => {
            eprintln!(
                "content delivery_pending guild={} purchase={purchase}",
                actor.guild_id()
            );
            Ok((
                format!(
                    "DeliveryPending: 購入ID {purchase} の配送を確認できません。無料再送または /content purchases から受け取れます。"
                ),
                vec![resend_button(actor, purchase)],
            ))
        }
    }
}

async fn confirm(
    http: &Http,
    data: &Data,
    user: UserId,
    id: &str,
    yes: bool,
    now: SystemTime,
    attachment_limit: u32,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let (guild, _) = id.split_once('-').ok_or("購入確認IDが不正です。")?;
    let guild: crate::domain::GuildId = guild.parse()?;
    let quote = {
        let mut confirmations = data.confirmations.lock().await;
        if confirmations.get(id).is_some_and(|quote| {
            quote.buyer_id().get() != user.get() || quote.item().guild_id() != guild
        }) {
            return Err(UseCaseError::PermissionDenied.into());
        }
        confirmations.remove(id)
    };
    if !yes && quote.is_some() {
        return Ok(("購入を取り消しました。課金していません。".into(), vec![]));
    }
    let actor = services::authenticate(http, GuildId::new(guild.get()), user).await?;
    if let Some(purchase) = data.shop.find_purchase_by_confirmation(actor, id).await? {
        return if yes {
            deliver(http, data, actor, purchase.buyer().id()).await
        } else {
            Ok((
                "決済済みです。この操作では返金されません。".into(),
                vec![resend_button(actor, purchase.buyer().id())],
            ))
        };
    }
    let quote = quote.filter(|quote| now < quote.expires_at())
        .ok_or("確認は処理中・取消済み・期限切れ、または再起動で失効しています。購入一覧を確認し、未購入なら商品から確認を作り直してください。")?;
    match quote.item().target() {
        ItemTarget::Content(file) => {
            if file.size_bytes() > u64::from(attachment_limit) {
                return Err("DMの添付上限を超えるため購入できません。課金していません。".into());
            }
            content::read_content(&data.content_dir, file).await?;
        }
        ItemTarget::Role(_) => roles::preflight(http, actor, quote.item()).await?,
    }
    let purchase = match data.shop.purchase(actor, &quote, now).await {
        Err(UseCaseError::Database(_)) => {
            eprintln!(
                "content settlement_pending guild={} item={} confirmation={id}",
                actor.guild_id(),
                quote.item().id()
            );
            return Ok((
                format!(
                    "決済結果を確認できません。確認ID: {id}\n新しい購入操作をせず、同じ確認から結果を照合してください。"
                ),
                vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(format!("content:confirm:{id}:yes"))
                        .label("決済結果を確認する")
                        .style(ButtonStyle::Primary),
                ])],
            ));
        }
        result => result?,
    };
    println!(
        "content purchase guild={} item={} confirmation={id} purchase={}",
        actor.guild_id(),
        quote.item().id(),
        purchase.buyer().id()
    );
    deliver(http, data, actor, purchase.buyer().id()).await
}

pub(super) async fn list(
    http: &Http,
    data: &Data,
    actor: Actor,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let purchases = data.shop.list_purchases(actor).await?;
    if purchases.is_empty() {
        return Ok(("未返金の購入はありません。".into(), vec![]));
    }
    let last = (purchases.len() - 1) / 5;
    let page = page.min(last);
    let guild_name = services::guild_name(http, actor.guild_id()).await?;
    let mut text = format!(
        "サーバー: {guild_name}\n購入一覧（{}/{}）",
        page + 1,
        last + 1
    );
    let mut buttons = Vec::new();
    for purchase in purchases.iter().skip(page * 5).take(5) {
        text.push_str(&format!(
            "\n{} / 商品ID {} / 購入ID {} / 支払額 {} NOT",
            purchase
                .item()
                .details()
                .name()
                .chars()
                .take(80)
                .collect::<String>(),
            purchase.item().id(),
            purchase.buyer().id(),
            -purchase.buyer().amount().get()
        ));
        buttons.push(
            CreateButton::new(format!(
                "content:resend:{}:{}",
                actor.guild_id(),
                purchase.buyer().id()
            ))
            .label(format!(
                "{} #{}",
                if matches!(purchase.item().target(), ItemTarget::Role(_)) {
                    "ロール再照合"
                } else {
                    "無料再送"
                },
                purchase.buyer().id()
            ))
            .style(ButtonStyle::Primary),
        );
    }
    let mut rows = vec![CreateActionRow::Buttons(buttons)];
    let mut navigation = Vec::new();
    for (target, label) in [
        (page.saturating_sub(1), "前へ"),
        ((page + 1).min(last), "次へ"),
    ] {
        if target != page {
            navigation.push(
                CreateButton::new(format!("content:purchases:{}:{target}", actor.guild_id()))
                    .label(label),
            );
        }
    }
    if !navigation.is_empty() {
        rows.push(CreateActionRow::Buttons(navigation));
    }
    Ok((text, rows))
}

pub(super) async fn handle_action(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
    now: SystemTime,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    match parts.as_slice() {
        ["content", "confirm", id, action @ ("yes" | "no")] if component.guild_id.is_none() => {
            confirm(
                http,
                data,
                component.user.id,
                id,
                *action == "yes",
                now,
                component.attachment_size_limit,
            )
            .await
        }
        [
            "content",
            kind @ ("buy" | "resend" | "purchases"),
            guild,
            target,
        ] => {
            let guild: crate::domain::GuildId = guild.parse()?;
            if component
                .guild_id
                .is_some_and(|current| current.get() != guild.get())
                || (*kind == "buy" && component.guild_id.is_none())
            {
                return Err("元の商品と同じサーバーから操作してください。".into());
            }
            let actor =
                services::authenticate(http, GuildId::new(guild.get()), component.user.id).await?;
            match *kind {
                "buy" => {
                    prepare(
                        http,
                        data,
                        actor,
                        target.parse()?,
                        format!("{guild}-{}", component.id),
                        now,
                    )
                    .await
                }
                "resend" => deliver(http, data, actor, target.parse()?).await,
                _ => list(http, data, actor, target.parse()?).await,
            }
        }
        _ => Err("購入操作が不正です。BotとのDMから確認してください。".into()),
    }
}

#[cfg(test)]
mod tests;
