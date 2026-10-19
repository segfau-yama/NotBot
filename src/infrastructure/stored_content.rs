use crate::domain::{Actor, ContentFile};

/// DMへの単一添付で扱う上限。Discordのユーザー／ギルド別の増枠は使わない。
pub const MAX_CONTENT_BYTES: u64 = 20 * 1024 * 1024;

/// 永続保存の完了を表す。外側の保存処理だけが生成し、未保存のメタデータと区別する。
#[derive(Debug, Clone)]
pub struct StoredContent {
    pub(super) actor: Actor,
    pub(super) file: ContentFile,
}

impl StoredContent {
    pub fn file(&self) -> &ContentFile {
        &self.file
    }
}
