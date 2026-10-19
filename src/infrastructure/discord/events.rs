use serenity::all::FullEvent;

use super::Data;
use crate::infrastructure::Error;

pub(super) async fn handle(event: &FullEvent) -> Result<(), Error> {
    if let FullEvent::Ready { .. } = event {
        println!("Discordに接続しました。/not が利用できます。");
    }
    Ok(())
}

pub(super) async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
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

fn public_error_message(error: &Error) -> String {
    // SDKエラーにはInteractionの秘密トークン付きURLが含まれ得る。
    if error.is::<serenity::Error>() {
        "Discordへの応答に失敗しました。".into()
    } else {
        error.to_string()
    }
}
