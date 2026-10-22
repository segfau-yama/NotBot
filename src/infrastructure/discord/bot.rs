use super::{commands, events};

use crate::{
    domain::{ChannelId, GuildId},
    infrastructure::{Error, config::Config, sqlite::SqliteDatabaseAccess},
};
use serenity::all::{Client, Command, CreateAllowedMentions, GatewayIntents};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

pub struct Data {
    pub(super) sqlite_not: crate::infrastructure::sqlite::not::NotRepository,
    pub(super) sqlite_shop: crate::infrastructure::sqlite::shop::ShopRepository,
    pub(super) discord_shop: super::shop::ShopRepository,
    #[cfg(test)]
    pub(super) database: SqliteDatabaseAccess,
    pub(super) shop_channels: HashMap<GuildId, ChannelId>,
    pub(super) download: reqwest::Client,
    pub(super) publication_locks:
        tokio::sync::Mutex<HashMap<crate::domain::ItemId, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    pub(super) drafts: tokio::sync::Mutex<HashMap<u64, commands::ListingDraft>>,
    pub(super) uploads: tokio::sync::Mutex<HashMap<u64, commands::Upload>>,
    pub(super) confirmations: tokio::sync::Mutex<HashMap<String, crate::domain::PurchaseQuote>>,
    pub(super) voice: tokio::sync::Mutex<
        HashMap<(serenity::all::GuildId, serenity::all::UserId), events::Attendance>,
    >,
}
impl Data {
    pub(super) fn new(
        database: SqliteDatabaseAccess,
        directory: PathBuf,
        shop_channels: HashMap<GuildId, ChannelId>,
    ) -> Result<Self, Error> {
        Ok(Self {
            sqlite_not: crate::infrastructure::sqlite::not::NotRepository::new(database.clone()),
            sqlite_shop: crate::infrastructure::sqlite::shop::ShopRepository::new(database.clone()),
            discord_shop: super::shop::ShopRepository::new(directory),
            #[cfg(test)]
            database,
            shop_channels,
            download: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|_| "添付取得の準備に失敗しました。")?,
            publication_locks: Default::default(),
            drafts: Default::default(),
            uploads: Default::default(),
            confirmations: Default::default(),
            voice: Default::default(),
        })
    }
}

pub type Context<'a> = poise::ApplicationContext<'a, Arc<Data>, Error>;

pub async fn run(config: &Config, database: SqliteDatabaseAccess) -> Result<(), Error> {
    let data = Arc::new(Data::new(
        database,
        config.content_dir().clone(),
        config.shop_channels().clone(),
    )?);

    let voice_data = data.clone();
    let commands = vec![commands::not(), commands::content(), commands::role()];
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
            | GatewayIntents::GUILD_MESSAGE_REACTIONS
            | GatewayIntents::GUILD_VOICE_STATES,
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

    let http = client.http.clone();
    let cache = client.cache.clone();
    let result: Result<(), Error> = tokio::select! {
            result = client.start() => result.map_err(Into::into),
            result = async {
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
    } => result.map_err(Into::into),
            _ = voice_data.reward_voice(&http, &cache) => Ok(()),
        };
    client.shard_manager.shutdown_all().await;
    result
}
