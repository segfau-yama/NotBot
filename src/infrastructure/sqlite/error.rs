use sea_orm::{DbErr, RuntimeErr, sqlx};

use crate::domain::{MoneyError, ValidationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DatabaseError {
    #[error("データベースを利用できません")]
    Unavailable,
    #[error("データの書き込みが競合しました")]
    Conflict,
    #[error("データの形式または整合性が不正です")]
    InvalidData,
    #[error("読み取り専用セッションには追記できません")]
    ReadOnly,
    #[error("確定結果を確認できません。同じ依頼IDで結果を照合してください")]
    OutcomeUnknown,
}

pub(super) fn map_commit_error(error: DbErr) -> DatabaseError {
    match DatabaseError::from(error) {
        DatabaseError::Unavailable => DatabaseError::OutcomeUnknown,
        error => error,
    }
}

impl From<DbErr> for DatabaseError {
    fn from(error: DbErr) -> Self {
        eprintln!("SQLite: {error}");
        match error {
            DbErr::Exec(RuntimeErr::SqlxError(error))
            | DbErr::Query(RuntimeErr::SqlxError(error))
            | DbErr::Conn(RuntimeErr::SqlxError(error)) => match error.as_ref() {
                sqlx::Error::Database(error) => {
                    let code = error.code().and_then(|code| code.parse::<i32>().ok());
                    match code {
                        Some(1555 | 2067) => DatabaseError::Conflict,
                        Some(code) => match code & 0xff {
                            5 | 6 => DatabaseError::Conflict,
                            8 => DatabaseError::ReadOnly,
                            1 | 11 | 18 | 19 | 20 | 26 => DatabaseError::InvalidData,
                            _ => DatabaseError::Unavailable,
                        },
                        None => DatabaseError::Unavailable,
                    }
                }
                sqlx::Error::ColumnDecode { .. }
                | sqlx::Error::Decode(_)
                | sqlx::Error::ColumnNotFound(_)
                | sqlx::Error::RowNotFound => DatabaseError::InvalidData,
                _ => DatabaseError::Unavailable,
            },
            DbErr::Type(_)
            | DbErr::TryIntoErr { .. }
            | DbErr::ConvertFromU64(_)
            | DbErr::UnpackInsertId => DatabaseError::InvalidData,
            _ => DatabaseError::Unavailable,
        }
    }
}

impl From<ValidationError> for DatabaseError {
    fn from(_: ValidationError) -> Self {
        Self::InvalidData
    }
}

impl From<MoneyError> for DatabaseError {
    fn from(_: MoneyError) -> Self {
        Self::InvalidData
    }
}
