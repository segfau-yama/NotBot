# 設計方針

Discord側とSQLite側にそれぞれRepositoryを置き、domainの型を介してデータを受け渡す。domainはSDK・DB・具体的なRepositoryを参照しない。業務規則を外部の形式から独立させることを、クリーンアーキテクチャの基準とする。[原典](https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html)

commandは入力取得、Repositoryの呼び出し、返信を担当する。単なる中継用のClient階層やUseCaseのstructは設けない。DB取得を伴う業務判断が必要になれば、その処理と必要最小限の永続化traitを内側に置く。

## Repository

| メソッド | Discord側の`ShopRepository` | SQLite側の`ShopRepository` |
| --- | --- | --- |
| `create` | 入力とDiscord APIの取得結果を商品データに変換する | 商品を保存し、商品IDを返す |
| `read` | 取得要求の商品IDをdomainの識別子に変換する | 商品IDから現在の商品を返す。存在しなければ`None` |
| `update` | 更新要求を商品IDと新しい商品データに変換する | 新しい商品履歴を保存する。対象がなければ`false` |
| `delete` | 削除要求の商品IDをdomainの識別子に変換する | 削除履歴を保存する。対象がなければ`false` |

Discord側のCRUDは各要求の取得・変換、SQLite側のCRUDは永続データへの操作を意味する。Discordのメッセージ投稿や削除とは区別する。同じメソッド名でも引数と戻り値は各境界に合わせ、共通CRUD traitにはしない。

- domainは商品の値と業務上の不変条件を持つ。フィールドを非公開にし、生成時の検証と読み取りだけを公開する。
- 文字列のparseやSDK型・DB行との変換は各Repositoryで行う。DBから復元する場合もdomainの生成経路を通す。
- Repository同士は参照しない。commandが`discord_shop`と`sqlite_shop`を呼び出し、domainの値を渡す。
- `update`・`delete`は論理的な操作名とする。SQLiteでは履歴をINSERTし、過去の値を残す。オブジェクトの再生成だけでDB更新を済ませたことにはしない。
- `Result`と`?`で失敗を伝播し、保存が確定してから成功を表示する。

この例のdomainでは「タイトルは空白だけにしない」「ギルドIDは0にしない」を不変条件とする。価格は非負の整数NOTとし、0も許可する。`ShopItem`は商品属性の値であり、永続的な識別子は`ShopItemId`として別に扱う。

## 疑似コード

```text
create(input):
    item = discord_shop.create(input)       // Discord API取得、形式変換、domain生成
    id = sqlite_shop.create(item)           // IDと商品履歴をまとめて保存
    商品IDを返信

read(input):
    id = discord_shop.read(input)
    item = sqlite_shop.read(id)
    商品または「見つからない」を返信

update(input):
    (id, item) = discord_shop.update(input)
    changed = sqlite_shop.update(id, item)
    更新結果を返信

delete(input):
    id = discord_shop.delete(input)
    changed = sqlite_shop.delete(id)
    削除結果を返信
```

## 活用方法(rustコード)

以下は独立したCargoプロジェクトとして動く例。`main`をcommandの実行入口とし、返信は標準出力で示す。Discordへの接続にはSerenity、実際のSQLite操作にはrusqliteを使う。プロジェクト本体のライブラリ変更は含意しない。

`Cargo.toml`:

```toml
[package]
name = "repository-example"
version = "0.1.0"
edition = "2021"

[dependencies]
rusqlite = { version = "=0.40.2", features = ["bundled"] }
serenity = { version = "=0.12.5", default-features = false, features = ["model", "rustls_backend"] }
tokio = { version = "1", features = ["macros", "rt"] }
```

`src/main.rs`:

```rust
type Error = Box<dyn std::error::Error + Send + Sync>;

mod domain {
    pub type ShopItemId = std::num::NonZeroU64;

    #[derive(Debug, PartialEq, Eq)]
    pub struct ShopItem {
        title: String,
        description: String,
        price: u64,
        guild_id: u64,
    }

    impl ShopItem {
        pub fn new(
            title: String,
            description: String,
            price: u64,
            guild_id: u64,
        ) -> Result<Self, &'static str> {
            if title.trim().is_empty() {
                return Err("タイトルが空です");
            }
            if guild_id == 0 {
                return Err("ギルドIDが不正です");
            }
            Ok(Self {
                title,
                description,
                price,
                guild_id,
            })
        }

        pub fn title(&self) -> &str {
            &self.title
        }
        pub fn description(&self) -> &str {
            &self.description
        }
        pub fn price(&self) -> u64 {
            self.price
        }
        pub fn guild_id(&self) -> u64 {
            self.guild_id
        }
    }
}

mod discord {
    use super::{
        domain::{ShopItem, ShopItemId},
        Error,
    };
    use serenity::{all::GuildId, http::Http};
    use std::num::NonZeroU64;

    pub struct ShopRepository<'a> {
        pub http: &'a Http,
    }

    impl ShopRepository<'_> {
        pub async fn create(
            &self,
            title: &str,
            description: &str,
            price: &str,
            guild: &str,
        ) -> Result<ShopItem, Error> {
            let price = price.parse()?;
            let guild_id: NonZeroU64 = guild.parse()?;
            let guild = self.http.get_guild(GuildId::new(guild_id.get())).await?;
            Ok(ShopItem::new(
                title.into(),
                description.into(),
                price,
                guild.id.get(),
            )?)
        }

        pub fn read(&self, id: &str) -> Result<ShopItemId, Error> {
            Ok(id.parse()?)
        }

        pub async fn update(
            &self,
            id: &str,
            title: &str,
            description: &str,
            price: &str,
            guild: &str,
        ) -> Result<(ShopItemId, ShopItem), Error> {
            let id = self.read(id)?;
            let item = self.create(title, description, price, guild).await?;
            Ok((id, item))
        }

        pub fn delete(&self, id: &str) -> Result<ShopItemId, Error> {
            self.read(id)
        }
    }
}

mod sqlite {
    use super::{
        domain::{ShopItem, ShopItemId},
        Error,
    };
    use rusqlite::{params, Connection, OptionalExtension};

    pub struct ShopRepository {
        connection: Connection,
    }

    impl ShopRepository {
        pub fn new(connection: Connection) -> Result<Self, Error> {
            connection.execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS shop_item (
                     id INTEGER PRIMARY KEY AUTOINCREMENT
                 );
                 CREATE TABLE IF NOT EXISTS shop_item_history (
                     revision INTEGER PRIMARY KEY,
                     item_id INTEGER NOT NULL REFERENCES shop_item(id),
                     title TEXT NOT NULL,
                     description TEXT NOT NULL,
                     price INTEGER NOT NULL CHECK (price >= 0),
                     guild_id TEXT NOT NULL,
                     deleted INTEGER NOT NULL CHECK (deleted IN (0, 1))
                 );",
            )?;
            Ok(Self { connection })
        }

        pub fn create(&mut self, item: &ShopItem) -> Result<ShopItemId, Error> {
            let transaction = self.connection.transaction()?;
            transaction.execute("INSERT INTO shop_item DEFAULT VALUES", [])?;
            let id = ShopItemId::new(transaction.last_insert_rowid().try_into()?)
                .ok_or("商品IDが不正です")?;
            transaction.execute(
                "INSERT INTO shop_item_history
                 (item_id, title, description, price, guild_id, deleted)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    i64::try_from(id.get())?,
                    item.title(),
                    item.description(),
                    i64::try_from(item.price())?,
                    item.guild_id().to_string()
                ],
            )?;
            transaction.commit()?;
            Ok(id)
        }

        pub fn read(&self, id: ShopItemId) -> Result<Option<ShopItem>, Error> {
            let values = self
                .connection
                .query_row(
                    "SELECT title, description, price, guild_id FROM shop_item_history
                 WHERE revision = (SELECT MAX(revision) FROM shop_item_history
                                   WHERE item_id = ?1) AND deleted = 0",
                    [i64::try_from(id.get())?],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            values
                .map(
                    |(title, description, price, guild_id)| -> Result<ShopItem, Error> {
                        Ok(ShopItem::new(
                            title,
                            description,
                            price.try_into()?,
                            guild_id.parse()?,
                        )?)
                    },
                )
                .transpose()
        }

        pub fn update(&self, id: ShopItemId, item: &ShopItem) -> Result<bool, Error> {
            let changed = self.connection.execute(
                "INSERT INTO shop_item_history
                 (item_id, title, description, price, guild_id, deleted)
                 SELECT ?1, ?2, ?3, ?4, ?5, 0
                 WHERE (SELECT deleted FROM shop_item_history WHERE item_id = ?1
                        ORDER BY revision DESC LIMIT 1) = 0",
                params![
                    i64::try_from(id.get())?,
                    item.title(),
                    item.description(),
                    i64::try_from(item.price())?,
                    item.guild_id().to_string()
                ],
            )?;
            Ok(changed == 1)
        }

        pub fn delete(&self, id: ShopItemId) -> Result<bool, Error> {
            let changed = self.connection.execute(
                "INSERT INTO shop_item_history
                 (item_id, title, description, price, guild_id, deleted)
                 SELECT item_id, title, description, price, guild_id, 1
                 FROM shop_item_history
                 WHERE revision = (SELECT MAX(revision) FROM shop_item_history
                                   WHERE item_id = ?1) AND deleted = 0",
                [i64::try_from(id.get())?],
            )?;
            Ok(changed == 1)
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let token = std::env::var("DISCORD_TOKEN")?;
    let guild = std::env::var("DISCORD_GUILD_ID")?;
    let http = serenity::http::Http::new(&token);
    let discord_shop = discord::ShopRepository { http: &http };
    let mut sqlite_shop = sqlite::ShopRepository::new(rusqlite::Connection::open_in_memory()?)?;

    let item = discord_shop.create("商品", "説明", "100", &guild).await?;
    let id = sqlite_shop.create(&item)?;
    let id_text = id.to_string();
    println!("作成した商品ID: {id}");

    let id = discord_shop.read(&id_text)?;
    println!("取得した商品: {:?}", sqlite_shop.read(id)?);

    let (id, item) = discord_shop
        .update(&id_text, "更新した商品", "説明", "200", &guild)
        .await?;
    println!("更新: {}", sqlite_shop.update(id, &item)?);

    let id = discord_shop.delete(&id_text)?;
    println!("削除: {}", sqlite_shop.delete(id)?);
    assert!(sqlite_shop.read(id)?.is_none());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 商品を更新すると最新の値を取得でき削除後は取得できない() -> Result<(), Error> {
        let http = serenity::http::Http::new("test-token");
        let discord_shop = discord::ShopRepository { http: &http };
        let mut sqlite_shop = sqlite::ShopRepository::new(rusqlite::Connection::open_in_memory()?)?;
        let item = domain::ShopItem::new("商品".into(), "説明".into(), 100, u64::MAX)?;
        let id = sqlite_shop.create(&item)?;
        assert_eq!(
            sqlite_shop.read(discord_shop.read(&id.to_string())?)?,
            Some(item)
        );

        let updated = domain::ShopItem::new("更新後".into(), "説明".into(), 0, u64::MAX)?;
        assert!(sqlite_shop.update(id, &updated)?);
        assert_eq!(sqlite_shop.read(id)?, Some(updated));
        assert!(sqlite_shop.delete(discord_shop.delete(&id.to_string())?)?);
        assert_eq!(sqlite_shop.read(id)?, None);

        let item = domain::ShopItem::new("再更新".into(), "説明".into(), 100, 1)?;
        assert!(!sqlite_shop.update(id, &item)?);
        assert!(!sqlite_shop.delete(id)?);
        Ok(())
    }

    #[test]
    fn タイトルが空白だけかギルド識別子がゼロなら商品を生成できない() {
        assert!(domain::ShopItem::new(" \n".into(), "".into(), 0, 1).is_err());
        assert!(domain::ShopItem::new("商品".into(), "".into(), 0, 0).is_err());
    }

    #[test]
    fn 商品履歴の保存に失敗したら商品識別子の発行も取り消される() -> Result<(), Error> {
        let mut sqlite_shop = sqlite::ShopRepository::new(rusqlite::Connection::open_in_memory()?)?;
        let item = domain::ShopItem::new("商品".into(), "".into(), u64::MAX, 1)?;
        assert!(sqlite_shop.create(&item).is_err());
        let item = domain::ShopItem::new("商品".into(), "".into(), 1, 1)?;
        assert_eq!(sqlite_shop.create(&item)?.get(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn 価格や商品識別子が不正なら外部取得前に要求を拒否する() {
        let http = serenity::http::Http::new("test-token");
        let discord_shop = discord::ShopRepository { http: &http };
        let create_error = discord_shop
            .create("商品", "", "abc", "1")
            .await
            .unwrap_err();
        let update_error = discord_shop
            .update("1", "商品", "", "abc", "1")
            .await
            .unwrap_err();
        assert!(create_error.is::<std::num::ParseIntError>());
        assert!(update_error.is::<std::num::ParseIntError>());
        assert!(discord_shop
            .read("0")
            .unwrap_err()
            .is::<std::num::ParseIntError>());
        assert!(discord_shop
            .delete("abc")
            .unwrap_err()
            .is::<std::num::ParseIntError>());
    }
}
```

SQLiteの価格は符号付き64bit整数へ変換するため、格納範囲外ならエラーになる。ギルドIDは`u64`の全域を保持できるようTEXTで保存する。生成時のID発行と初回履歴はトランザクションでまとめ、更新・削除は存在確認を含む単一INSERTで行う。`read`は最新履歴の削除状態を判定し、過去の値を再表示しない。

`cargo test`はDiscordに接続せずに実行できる。`cargo run`では`DISCORD_TOKEN`と、Botが参加しているギルドの`DISCORD_GUILD_ID`を環境変数に設定する。実行するとギルドをDiscord APIから取得し、メモリ上の実SQLiteでCRUDを実行する。

ギルドの取得成功は実行者の出品・更新・削除権限を証明しない。実コマンドでは実行者の情報を取得し、業務上の認可を通してから保存する。この例はデータ変換と永続化の流れを対象とする。

APIの参照先: [Serenity Http::get_guild](https://docs.rs/serenity/0.12.5/serenity/http/struct.Http.html#method.get_guild)、[rusqlite Connection](https://docs.rs/rusqlite/0.40.2/rusqlite/struct.Connection.html)。
