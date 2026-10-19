//! Bot専用の永続ディスクへの保存。ネットワーク取得は呼び出し側で行う。
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::domain::{Actor, ContentFile, ValidationError};

use crate::infrastructure::{MAX_CONTENT_BYTES, StoredContent};

pub(super) fn validate_attachment(
    file: &serenity::all::Attachment,
) -> Result<(), crate::infrastructure::Error> {
    ContentFile::try_new("preview", &file.filename, file.size.into())?;
    if u64::from(file.size) > MAX_CONTENT_BYTES || file.filename.len() > 255 {
        return Err("ファイルは有効な名前で、1バイト〜20 MiBにしてください。".into());
    }
    let url = reqwest::Url::parse(&file.url).map_err(|_| "添付URLが不正です。")?;
    if url.scheme() != "https"
        || url.host_str() != Some("cdn.discordapp.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.path().starts_with("/attachments/")
            || url.path().starts_with("/ephemeral-attachments/"))
    {
        return Err("Discordの添付ファイルだけを受け付けます。".into());
    }
    Ok(())
}

pub(super) async fn download_attachment(
    client: &reqwest::Client,
    file: &serenity::all::Attachment,
) -> Result<Vec<u8>, crate::infrastructure::Error> {
    validate_attachment(file)?;
    let response = client
        .get(&file.url)
        .send()
        .await
        .map_err(|_| "添付ファイルを取得できませんでした。")?;
    read_attachment(response, file.size.into()).await
}

pub(super) async fn read_attachment(
    mut response: reqwest::Response,
    expected: u64,
) -> Result<Vec<u8>, crate::infrastructure::Error> {
    if !response.status().is_success() {
        return Err("添付ファイルを取得できませんでした。".into());
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_CONTENT_BYTES)
    {
        return Err(ContentStorageError::TooLarge.into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "添付ファイルの読取りに失敗しました。")?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_CONTENT_BYTES {
            return Err(ContentStorageError::TooLarge.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() || bytes.len() as u64 != expected {
        return Err("添付ファイルのサイズが一致しません。".into());
    }
    Ok(bytes)
}

pub(super) async fn read_content(
    directory: &Path,
    file: &ContentFile,
) -> Result<Vec<u8>, crate::infrastructure::Error> {
    let directory = directory.to_path_buf();
    let file = file.clone();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, crate::infrastructure::Error> {
        let directory = fs::canonicalize(directory)?;
        let path = directory.join(file.key());
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || fs::canonicalize(&path)? != path {
            return Err(ContentStorageError::UnsafePath.into());
        }
        if file.size_bytes() > MAX_CONTENT_BYTES || metadata.len() != file.size_bytes() {
            return Err("配送できるファイルサイズではありません。".into());
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_CONTENT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != file.size_bytes() {
            return Err("保存済みファイルのサイズが一致しません。".into());
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| "コンテンツの読出し処理に失敗しました。")?
    .map_err(|_| "コンテンツを読み出せませんでした。".into())
}

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
