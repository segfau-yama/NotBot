use notbot::infrastructure::{Error, config::Config, discord, sqlite::SqliteDatabaseAccess};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::from_env()?;
    let database = SqliteDatabaseAccess::open(config.database_path()).await?;
    let result = discord::run(config.discord_token(), database.clone())
        .await
        .map_err(|_| "Discordの起動または接続に失敗しました");
    database.close().await?;
    result.map_err(Into::into)
}
