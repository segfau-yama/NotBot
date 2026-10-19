use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serenity::all::*;

use super::{
    Data, content,
    events::public_error_message,
    listings::{self, ListingDraft},
    services,
};
use crate::{domain::GuildId as DomainGuildId, infrastructure::Error};

#[derive(Clone)]
pub(super) struct Upload {
    id: String,
    attachment: Attachment,
    guild: Option<DomainGuildId>,
    expires_at: Instant,
}

fn pending<'a>(
    uploads: &'a HashMap<u64, Upload>,
    user: UserId,
    id: &str,
) -> Result<&'a Upload, Error> {
    uploads
        .get(&user.get())
        .filter(|upload| upload.id == id && Instant::now() < upload.expires_at)
        .ok_or_else(|| "出品入力が失効しています。DMへファイルを送り直してください。".into())
}

pub(super) async fn receive(http: &Http, data: &Data, message: &Message) -> Result<(), Error> {
    if message.guild_id.is_some() || message.author.bot || message.attachments.is_empty() {
        return Ok(());
    }
    let valid = if message.attachments.len() == 1 {
        content::validate_attachment(&message.attachments[0])
    } else {
        Err("出品するファイルは1つずつ送ってください。".into())
    };
    if let Err(error) = valid {
        message
            .channel_id
            .send_message(
                http,
                CreateMessage::new()
                    .content(public_error_message(&error))
                    .allowed_mentions(CreateAllowedMentions::new()),
            )
            .await?;
        return Ok(());
    }
    let id = message.id.to_string();
    {
        let mut uploads = data.uploads.lock().await;
        uploads.retain(|_, upload| Instant::now() < upload.expires_at);
        uploads.insert(
            message.author.id.get(),
            Upload {
                id: id.clone(),
                attachment: message.attachments[0].clone(),
                guild: None,
                expires_at: Instant::now() + Duration::from_secs(300),
            },
        );
    }
    data.drafts.lock().await.remove(&message.author.id.get());
    let buttons = vec![
        CreateButton::new(format!("content:post:{id}:start"))
            .label("このファイルを出品する")
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("content:post:{id}:cancel")).label("キャンセル"),
    ];
    message.channel_id.send_message(http, CreateMessage::new()
        .content("ファイルを受け取りました。5分以内に出品内容を入力してください。最後の確認まで公開されません。")
        .components(vec![CreateActionRow::Buttons(buttons)]).allowed_mentions(CreateAllowedMentions::new())).await?;
    Ok(())
}

async fn servers(
    http: &Http,
    data: &Data,
    user: UserId,
    id: &str,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    pending(&*data.uploads.lock().await, user, id)?;
    let mut guilds: Vec<_> = data.shop_channels.keys().copied().collect();
    guilds.sort_by_key(|guild| guild.get());
    let last = guilds.len().saturating_sub(1) / 25;
    let page = page.min(last);
    let mut options = Vec::new();
    // shortcut: 設定済みサーバーを25件ずつ順に照会する。待ち時間が問題になったら並列数を制限して照会する。
    for id in guilds.into_iter().skip(page * 25).take(25) {
        let guild_id = GuildId::new(id.get());
        let Ok(guild) = http.get_guild(guild_id).await else {
            continue;
        };
        let Ok(member) = http.get_member(guild_id, user).await else {
            continue;
        };
        if services::authenticated_actor(&guild, &member, user).is_ok() {
            options.push(CreateSelectMenuOption::new(
                guild.name.chars().take(50).collect::<String>(),
                id.to_string(),
            ));
        }
    }
    let mut rows = Vec::new();
    if !options.is_empty() {
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(
                format!("content:post:{id}:server"),
                CreateSelectMenuKind::String { options },
            )
            .placeholder("出品先サーバー"),
        ));
    }
    let mut buttons = Vec::new();
    for (target, label) in [
        (page.saturating_sub(1), "前へ"),
        ((page + 1).min(last), "次へ"),
    ] {
        if target != page {
            buttons
                .push(CreateButton::new(format!("content:post:{id}:page:{target}")).label(label));
        }
    }
    buttons.push(CreateButton::new(format!("content:post:{id}:cancel")).label("キャンセル"));
    rows.push(CreateActionRow::Buttons(buttons));
    Ok((
        format!(
            "出品先を選択してください（{}/{}）。候補がなければ販売チャンネル設定と所属を確認してください。",
            page + 1,
            last + 1
        ),
        rows,
    ))
}

async fn form(
    data: &Data,
    user: UserId,
    id: &str,
    selected: Option<&str>,
) -> Result<CreateModal, Error> {
    let mut uploads = data.uploads.lock().await;
    let upload = pending(&uploads, user, id)?;
    let guild = selected
        .map(str::parse::<DomainGuildId>)
        .transpose()?
        .or(upload.guild)
        .filter(|guild| data.shop_channels.contains_key(guild))
        .ok_or("出品先が設定されていません。")?;
    uploads.get_mut(&user.get()).expect("checked upload").guild = Some(guild);
    Ok(
        CreateModal::new(format!("content:post:{id}:form:{guild}"), "出品内容の入力").components(
            vec![
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Short, "商品名", "title")
                        .min_length(1)
                        .max_length(256),
                ),
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Paragraph, "説明", "description")
                        .required(false)
                        .max_length(4096),
                ),
                CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Short, "価格（正のNOT整数）", "price")
                        .min_length(1)
                        .max_length(19),
                ),
            ],
        ),
    )
}

pub(super) async fn handle_component(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    if component.guild_id.is_none() {
        let hinted = if let ["content", "post", id, "start"] = parts.as_slice() {
            pending(&*data.uploads.lock().await, component.user.id, id)
                .is_ok_and(|upload| upload.guild.is_some())
        } else {
            false
        };
        let modal = match parts.as_slice() {
            ["content", "post", id, "server"] => match &component.data.kind {
                ComponentInteractionDataKind::StringSelect { values } if values.len() == 1 => {
                    Some(form(data, component.user.id, id, Some(&values[0])).await)
                }
                _ => Some(Err("出品先を1つ選んでください。".into())),
            },
            ["content", "post", id, "start"] if hinted => {
                Some(form(data, component.user.id, id, None).await)
            }
            _ => None,
        };
        if let Some(modal) = modal {
            let response = match modal {
                Ok(modal) => CreateInteractionResponse::Modal(modal),
                Err(error) => CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(public_error_message(&error))
                        .ephemeral(true),
                ),
            };
            return Ok(component.create_response(http, response).await?);
        }
    }
    component
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let result = action(http, data, component).await;
    let (text, buttons) = result.unwrap_or_else(|error| (public_error_message(&error), vec![]));
    component
        .edit_response(
            http,
            EditInteractionResponse::new()
                .content(text)
                .components(buttons)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
    Ok(())
}

async fn action(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(String, Vec<CreateActionRow>), Error> {
    let parts: Vec<_> = component.data.custom_id.split(':').collect();
    if component.guild_id.is_some() {
        return Err("BotとのDMから操作してください。".into());
    }
    match parts.as_slice() {
        ["content", "post", id, "start"] => servers(http, data, component.user.id, id, 0).await,
        ["content", "post", id, "page", page] => {
            servers(http, data, component.user.id, id, page.parse()?).await
        }
        ["content", "post", id, "cancel"] => {
            let mut uploads = data.uploads.lock().await;
            pending(&uploads, component.user.id, id)?;
            uploads.remove(&component.user.id.get());
            Ok((
                "出品入力を取り消しました。ファイルは保存・公開していません。".into(),
                vec![],
            ))
        }
        _ => Err("出品入力が不正です。".into()),
    }
}

async fn preview(
    http: &Http,
    data: &Data,
    modal: &ModalInteraction,
) -> Result<poise::CreateReply, Error> {
    if modal.guild_id.is_some() {
        return Err("BotとのDMから操作してください。".into());
    }
    let parts: Vec<_> = modal.data.custom_id.split(':').collect();
    let ["content", "post", id, "form", guild] = parts.as_slice() else {
        return Err("出品フォームが不正です。".into());
    };
    let guild: DomainGuildId = guild.parse()?;
    let upload = pending(&*data.uploads.lock().await, modal.user.id, id)?.clone();
    if upload.guild != Some(guild) {
        return Err("出品先が変更されています。フォームを開き直してください。".into());
    }
    let mut fields = HashMap::new();
    for row in &modal.data.components {
        for field in &row.components {
            if let ActionRowComponent::InputText(input) = field
                && fields
                    .insert(
                        input.custom_id.as_str(),
                        input.value.as_deref().unwrap_or(""),
                    )
                    .is_some()
            {
                return Err("フォームの項目が重複しています。".into());
            }
        }
    }
    let title = fields.get("title").ok_or("商品名がありません。")?;
    let description = fields.get("description").copied().unwrap_or("");
    let price = fields.get("price").ok_or("価格がありません。")?;
    let actor = services::authenticate(http, GuildId::new(guild.get()), modal.user.id).await?;
    let (_, name) = listings::selling_channel(http, data, actor).await?;
    let draft = ListingDraft::new(
        id.to_string(),
        actor,
        title.to_string(),
        description.into(),
        price,
        upload.attachment,
    )?;
    let mut uploads = data.uploads.lock().await;
    if pending(&uploads, modal.user.id, id)?.guild != Some(guild) {
        return Err("出品先が変更されています。フォームを開き直してください。".into());
    }
    uploads.remove(&modal.user.id.get());
    Ok(listings::remember_draft(data, draft, &name).await)
}

pub(super) async fn handle_modal(
    http: &Http,
    data: &Data,
    modal: &ModalInteraction,
) -> Result<(), Error> {
    modal
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let reply = preview(http, data, modal).await.unwrap_or_else(|error| {
        poise::CreateReply::default().content(public_error_message(&error))
    });
    modal
        .edit_response(
            http,
            reply
                .allowed_mentions(CreateAllowedMentions::new())
                .to_slash_initial_response_edit(EditInteractionResponse::new()),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
