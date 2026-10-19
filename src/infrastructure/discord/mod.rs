pub mod commands;
pub mod content;
mod events;
mod services;

use serenity::all::{Client, Command, CreateAllowedMentions, GatewayIntents};

use super::{
    Error,
    sqlite::{SqliteDatabaseAccess, not::NotUseCase},
};

pub struct Data {
    not: NotUseCase,
}

pub type Context<'a> = poise::ApplicationContext<'a, Data, Error>;

pub async fn run(token: &str, database: SqliteDatabaseAccess) -> Result<(), Error> {
    let commands = vec![commands::not()];
    let registration = poise::builtins::create_application_commands(&commands);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            allowed_mentions: Some(CreateAllowedMentions::new()),
            initialize_owners: false,
            event_handler: |_, event| Box::pin(events::handle(event)),
            on_error: |error| Box::pin(events::on_error(error)),
            ..Default::default()
        })
        .setup(move |_, _, _| {
            Box::pin(async move {
                Ok(Data {
                    not: NotUseCase::new(database),
                })
            })
        })
        .build();
    let mut client = Client::builder(token, GatewayIntents::GUILDS)
        .framework(framework)
        .await?;

    // READY前の登録なので、HTTPクライアントへアプリケーションIDを設定する。
    let application = client.http.get_current_application_info().await?;
    client.http.set_application_id(application.id);
    // 他のコマンドを全件置換せず、Poiseが定義したコマンドだけを作成・更新する。
    for command in registration {
        Command::create_global_command(&client.http, command).await?;
    }

    let result: Result<(), Error> = tokio::select! {
        result = client.start() => result.map_err(Into::into),
        result = shutdown_signal() => result.map_err(Into::into),
    };
    client.shard_manager.shutdown_all().await;
    result
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
