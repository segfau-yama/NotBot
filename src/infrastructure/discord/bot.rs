use std::collections::HashMap;

use serenity::all::{Client, Command, CreateAllowedMentions, GatewayIntents};
use tokio::sync::Mutex;

use super::{Data, commands, events, listings, roles};
use crate::infrastructure::{
    Error,
    config::Config,
    sqlite::{SqliteDatabaseAccess, not::NotUseCase, shop::ShopUseCase},
};

pub async fn run(config: &Config, database: SqliteDatabaseAccess) -> Result<(), Error> {
    let data = Data {
        not: NotUseCase::new(database.clone()),
        shop: ShopUseCase::new(database.clone()),
        database,
        content_dir: config.content_dir().clone(),
        shop_channels: config.shop_channels().clone(),
        download: listings::download_client()?,
        publication_locks: Mutex::new(HashMap::new()),
        drafts: Mutex::new(HashMap::new()),
        uploads: Mutex::new(HashMap::new()),
        confirmations: Mutex::new(HashMap::new()),
    };
    let commands = vec![commands::not(), commands::content(), roles::role()];
    let registration = poise::builtins::create_application_commands(&commands);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            allowed_mentions: Some(CreateAllowedMentions::new()),
            initialize_owners: false,
            event_handler: |ctx, event| {
                Box::pin(events::handle(ctx.serenity_context, event, ctx.user_data))
            },
            on_error: |error| Box::pin(events::on_error(error)),
            ..Default::default()
        })
        .setup(move |_, _, _| Box::pin(async move { Ok(data) }))
        .build();
    let mut client = Client::builder(
        config.discord_token(),
        GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS,
    )
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
