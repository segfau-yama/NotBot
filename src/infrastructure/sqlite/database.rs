use std::{fs, path::Path, time::Duration};

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, RuntimeErr, SqliteTransactionMode,
    SqlxSqliteConnector, Statement, TransactionOptions, TransactionTrait,
    sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use super::{DatabaseError, SqliteSession, error::map_commit_error};

#[derive(Clone)]
pub struct SqliteDatabaseAccess {
    connection: DatabaseConnection,
}

impl SqliteDatabaseAccess {
    /// DBファイルと親ディレクトリを作成し、必要なマイグレーションを適用する。
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err(DatabaseError::InvalidData);
        }
        let path = std::path::absolute(path).map_err(|error| {
            eprintln!("SQLite path: {error}");
            DatabaseError::Unavailable
        })?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                eprintln!("SQLite directory: {error}");
                DatabaseError::Unavailable
            })?;
        }
        // URL文字列を組み立てず、空白や ? # を含むパスもそのまま渡す。
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(|error| {
                DatabaseError::from(DbErr::Conn(RuntimeErr::SqlxError(error.into())))
            })?;
        let database = Self {
            connection: SqlxSqliteConnector::from_sqlx_sqlite_pool(pool),
        };
        database.migrate().await?;
        Ok(database)
    }

    pub async fn close(self) -> Result<(), DatabaseError> {
        self.connection.close().await.map_err(Into::into)
    }

    async fn migrate(&self) -> Result<(), DatabaseError> {
        let transaction = self.connection.begin_with_options(write_options()).await?;
        let row = transaction
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA user_version",
            ))
            .await?
            .ok_or(DatabaseError::InvalidData)?;
        let version: i64 = row.try_get("", "user_version")?;
        if !(0..=2).contains(&version) {
            return Err(DatabaseError::InvalidData);
        }
        if version == 0 {
            transaction
                .execute_unprepared(include_str!("001_initial.sql"))
                .await?;
        }
        if version < 2 {
            transaction
                .execute_unprepared(include_str!("002_rewards.sql"))
                .await?;
            transaction
                .execute_unprepared("PRAGMA user_version = 2")
                .await?;
        }
        transaction.commit().await.map_err(map_commit_error)
    }
}

fn write_options() -> TransactionOptions {
    TransactionOptions {
        sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
        ..Default::default()
    }
}

impl SqliteDatabaseAccess {
    pub async fn begin_read(&self) -> Result<SqliteSession, DatabaseError> {
        let transaction = self.connection.begin().await?;
        Ok(SqliteSession::new(transaction, false))
    }

    pub async fn begin_write(&self) -> Result<SqliteSession, DatabaseError> {
        let transaction = self.connection.begin_with_options(write_options()).await?;
        Ok(SqliteSession::new(transaction, true))
    }
}
