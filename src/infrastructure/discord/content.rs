//! Bot専用の永続ディスクへの保存。ネットワーク取得は呼び出し側で行う。
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::domain::{Actor, ContentFile, ValidationError};

use crate::infrastructure::{MAX_CONTENT_BYTES, StoredContent};

#[derive(Debug, thiserror::Error)]
pub enum ContentStorageError {
    #[error("{0}")]
    Validation(#[from] ValidationError),
    #[error("ファイルが配送可能なサイズを超えています")]
    TooLarge,
    #[error("同じ出品操作に異なるファイルが指定されています")]
    Conflict,
    #[error("保存先が通常のファイルではありません")]
    UnsafePath,
    #[error("コンテンツの保存に失敗しました")]
    Io(#[from] io::Error),
}

/// 同じ下書きは同じrequest_idを渡す。IDはBotの入力処理が発行し、利用者に指定させない。
/// 同期I/Oなので非同期のイベント処理からはspawn_blockingで呼ぶ。DBセッションは渡さない。
pub fn store_content(
    directory: &Path,
    actor: Actor,
    request_id: &str,
    filename: &str,
    source: impl Read,
) -> Result<StoredContent, ContentStorageError> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(ValidationError::InvalidEventKey.into());
    }
    let key = format!(
        "content-{}-{}-{request_id}",
        actor.guild_id(),
        actor.user_id()
    );
    // 入力不正なら、読取りやディレクトリ作成前に拒否する。
    ContentFile::try_new(&key, filename, 1)?;
    if filename.len() > 255 {
        return Err(ValidationError::InvalidFileName.into());
    }
    // ponytail: 上限20MiBのバッファで比較する。同時出品のメモリ使用が問題になれば逐次比較へ移す。
    let mut bytes = Vec::new();
    source.take(MAX_CONTENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONTENT_BYTES {
        return Err(ContentStorageError::TooLarge);
    }
    let file = ContentFile::try_new(&key, filename, bytes.len() as u64)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let directory = fs::canonicalize(directory)?;
    let path = directory.join(&key);

    // 完成ファイルは置き換えない。並行した再試行もhard_linkの排他的な作成で調停する。
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut output) = loop {
        let temporary = directory.join(format!(
            ".pending-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(output) => break (temporary, output),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| {
        output.write_all(&bytes)?;
        output.sync_all()?;
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_file() || fs::canonicalize(&path)? != path {
                    return Err(ContentStorageError::UnsafePath);
                }
                if metadata.len() != file.size_bytes() {
                    return Err(ContentStorageError::Conflict);
                }
                let existing = File::open(&path)?;
                let mut saved = Vec::new();
                (&existing)
                    .take(MAX_CONTENT_BYTES + 1)
                    .read_to_end(&mut saved)?;
                if saved != bytes {
                    return Err(ContentStorageError::Conflict);
                }
                existing.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        // ファイル本体だけでなくディレクトリエントリも永続化してからDBへ渡す。
        File::open(&directory)?.sync_all()?;
        Ok(StoredContent { actor, file })
    })();
    drop(output);
    if let Err(error) = fs::remove_file(&temporary) {
        eprintln!("Content temporary file cleanup: {error}");
    }
    // DBへ渡す前後とも完成ファイルは消さない。結果不明時も同じキーで照合できる。
    result
}
