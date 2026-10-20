use super::{commands, events, services};

use crate::{
    domain::{ChannelId, GuildId, ItemId, PurchaseQuote},
    infrastructure::{
        Error,
        config::Config,
        sqlite::{SqliteDatabaseAccess, not::NotUseCase, shop::ShopUseCase},
    },
};
use serenity::all::{Client, Command, CreateAllowedMentions, GatewayIntents};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;

pub struct Data {
    pub(super) not: NotUseCase,
    pub(super) shop: ShopUseCase,
    pub(super) database: SqliteDatabaseAccess,
    pub(super) content_dir: PathBuf,
    pub(super) shop_channels: HashMap<GuildId, ChannelId>,
    pub(super) download: reqwest::Client,
    pub(super) publication_locks: Mutex<HashMap<ItemId, Weak<Mutex<()>>>>,
    pub(super) drafts: Mutex<HashMap<u64, services::ListingDraft>>,
    pub(super) uploads: Mutex<HashMap<u64, services::Upload>>,
    pub(super) confirmations: Mutex<HashMap<String, PurchaseQuote>>,
    pub(super) voice:
        Mutex<HashMap<(serenity::all::GuildId, serenity::all::UserId), events::Attendance>>,
}

impl Data {
    pub(super) async fn publication_lock(&self, item: ItemId) -> Arc<Mutex<()>> {
        let mut locks = self.publication_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&item).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(item, Arc::downgrade(&lock));
        lock
    }
}

pub type Context<'a> = poise::ApplicationContext<'a, Arc<Data>, Error>;

pub async fn run(config: &Config, database: SqliteDatabaseAccess) -> Result<(), Error> {
    let data = Arc::new(Data {
        not: NotUseCase::new(database.clone()),
        shop: ShopUseCase::new(database.clone()),
        database,
        content_dir: config.content_dir().clone(),
        shop_channels: config.shop_channels().clone(),
        download: services::download_client()?,
        publication_locks: Mutex::new(HashMap::new()),
        drafts: Mutex::new(HashMap::new()),
        uploads: Mutex::new(HashMap::new()),
        confirmations: Mutex::new(HashMap::new()),
        voice: Mutex::new(HashMap::new()),
    });
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
            _ = events::reward_voice(&http, &cache, &voice_data) => Ok(()),
        };
    client.shard_manager.shutdown_all().await;
    result
}
