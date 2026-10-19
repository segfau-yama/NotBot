use sea_orm::DatabaseTransaction;

use super::{DatabaseError, error::map_commit_error};

pub struct SqliteSession {
    transaction: Option<DatabaseTransaction>,
    pub(super) writable: bool,
    failure: Option<DatabaseError>,
}

impl SqliteSession {
    pub(super) fn new(transaction: DatabaseTransaction, writable: bool) -> Self {
        Self {
            transaction: Some(transaction),
            writable,
            failure: None,
        }
    }

    pub(super) fn take_transaction(&mut self) -> Result<DatabaseTransaction, DatabaseError> {
        self.transaction
            .take()
            .ok_or(self.failure.unwrap_or(DatabaseError::Unavailable))
    }

    pub(super) async fn finish_operation<T: Send>(
        &mut self,
        transaction: DatabaseTransaction,
        result: Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        match result {
            Ok(value) => {
                self.transaction = Some(transaction);
                Ok(value)
            }
            Err(error) => {
                self.failure = Some(error);
                transaction.rollback().await?;
                Err(error)
            }
        }
    }

    pub async fn commit(mut self) -> Result<(), DatabaseError> {
        self.take_transaction()?
            .commit()
            .await
            .map_err(map_commit_error)
    }

    pub async fn rollback(mut self) -> Result<(), DatabaseError> {
        self.take_transaction()?
            .rollback()
            .await
            .map_err(Into::into)
    }
}
