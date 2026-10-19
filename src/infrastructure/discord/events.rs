use std::time::SystemTime;

use serenity::all::*;

use super::Data;
use crate::infrastructure::Error;

pub(super) async fn handle(ctx: &Context, event: &FullEvent, data: &Data) -> Result<(), Error> {
    let now = SystemTime::now();
    if let FullEvent::Ready { .. } = event {
        println!("Discordに接続しました。/not・/content・/role が利用できます。");
    }
    if let FullEvent::GuildCreate { guild, .. } = event {
        super::roles::reconcile(&ctx.http, data, guild.id).await?;
    }
    if let FullEvent::Message { new_message } = event {
        super::rewards::message(&ctx.http, data, new_message, now).await?;
        super::posting::receive(&ctx.http, data, new_message).await?;
    }
    if let FullEvent::ReactionAdd { add_reaction } = event {
        super::rewards::reaction(&ctx.http, data, add_reaction, now).await?;
    }
    if let FullEvent::InteractionCreate {
        interaction: Interaction::Modal(modal),
    } = event
        && modal.data.custom_id.starts_with("content:post:")
    {
        super::posting::handle_modal(&ctx.http, data, modal).await?;
    }
    if let FullEvent::InteractionCreate {
        interaction: Interaction::Component(component),
    } = event
    {
        handle_component(&ctx.http, data, component).await?;
    }
    Ok(())
}

pub(super) async fn handle_component(
    http: &Http,
    data: &Data,
    component: &ComponentInteraction,
) -> Result<(), Error> {
    let now = SystemTime::now();
    let kind = component
        .data
        .custom_id
        .strip_prefix("content:")
        .and_then(|id| id.split(':').next());
    let listing = matches!(kind, Some("draft" | "republish"));
    if kind == Some("post") {
        return super::posting::handle_component(http, data, component).await;
    }
    if !listing && !matches!(kind, Some("buy" | "confirm" | "resend" | "purchases")) {
        return Ok(());
    }
    component
        .create_response(
            http,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let result = if listing {
        super::listings::handle_action(http, data, component).await
    } else {
        super::purchases::handle_action(http, data, component, now).await
    };
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

pub(super) async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    if let poise::FrameworkError::EventHandler { event, error, .. } = &error {
        eprintln!(
            "Discord: イベントの処理に失敗しました。event={} {}",
            event.snake_case_name(),
            error_summary(error)
        );
        return;
    }
    if let Some(poise::Context::Application(ctx)) = error.ctx() {
        let content = match &error {
            poise::FrameworkError::Command { error, .. } => public_error_message(error),
            _ => "入力を確認して、もう一度操作してください。".into(),
        };
        if ctx
            .send(
                poise::CreateReply::default()
                    .content(content)
                    .ephemeral(true),
            )
            .await
            .is_err()
        {
            eprintln!(
                "Discord: 応答に失敗しました（操作ID: {}）",
                ctx.interaction.id
            );
        }
    } else {
        eprintln!("Discord: イベントの処理に失敗しました。");
    }
}

pub(super) fn error_summary(error: &Error) -> String {
    // URL・応答本文・イベント全体は秘密情報を含み得るため出力しない。
    match error.downcast_ref::<serenity::Error>() {
        Some(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(response))) => {
            format!(
                "HTTP={} code={}",
                response.status_code.as_u16(),
                response.error.code
            )
        }
        Some(serenity::Error::Json(error)) => {
            format!(
                "Discord JSON解析エラー line={} column={}",
                error.line(),
                error.column()
            )
        }
        Some(_) => "Discord SDK・通信エラー".into(),
        None => {
            if let Some(error) = error.downcast_ref::<crate::infrastructure::UseCaseError>() {
                format!("{error:?}")
            } else if let Some(error) = error.downcast_ref::<std::io::Error>() {
                format!("ファイルI/Oエラー kind={:?}", error.kind())
            } else {
                "内部エラー（詳細非表示）".into()
            }
        }
    }
}

pub(super) fn public_error_message(error: &Error) -> String {
    // SDKエラーにはInteractionの秘密トークン付きURLが含まれ得る。
    if error.is::<serenity::Error>() {
        "Discordへの応答に失敗しました。".into()
    } else {
        error.to_string()
    }
}
