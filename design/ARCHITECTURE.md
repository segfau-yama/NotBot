# 構成

ドメインとインフラの2層とする。ドメインは検証済みの値・商品・取引・結果を保持し、読み取りだけを公開する。型変換・検証・計算・I/Oはインフラで行う。 domainの全フィールドはprivateにし、crate内の生成メソッドは受け取った値の格納だけを行う。生成前の変換・検証はRepositoryの責務とし、setterや可変参照を返すgetterは置かない。

```text
src/
  domain.rs              # データ型・読み取り操作・保存済みファイル情報
  infrastructure/
    mod.rs                # モジュール宣言・再公開
    config.rs
    types.rs              # エラー・定数・共通の値変換（インフラ内だけで使用）
    discord/
      mod.rs              # モジュール宣言・再公開
      bot.rs              # Poiseの起動・共有状態とRepositoryの組み立て・終了
      commands.rs         # コマンド・出品と購入の対話・送信・ロール操作
      events.rs           # Gateway・ボタンの受付・投稿と通話の報酬
      not.rs              # NotRepository: Discordから所属・権限を取得しActorへ変換
      shop.rs             # ShopRepository: 商品入力をItemDetailsへ変換
      content.rs          # ShopRepositoryの添付取得・ファイル保存と読出し
    sqlite/
      mod.rs              # モジュール宣言・再公開
      database.rs         # 接続・移行・取引境界・SQL
      not.rs              # NotRepository: 残高・調整・報酬・譲渡・取引の変換
      shop.rs             # ShopRepository: 商品・公開記録・購入・返金・結果の整合性
      001_initial.sql
      002_rewards.sql
  lib.rs
  main.rs
```

Repositoryは外部データの取得・変換・保存を担当する。各Repositoryは他のRepositoryを保持・参照・呼び出ししない。`commands`／`events` がDiscord側のRepositoryから受け取ったドメイン値をSQLite側のRepositoryへ渡し、戻ったドメイン値を使って返信する。ドメイン自身は処理の進行やRepositoryの呼び出しを担当しない。

Discordの `NotRepository` はSerenityのHTTPクライアントを参照し、本人・所属・権限から `Actor` を生成する。`ShopRepository` は商品入力を `ItemDetails` へ変換し、保存先にファイルを保存して `StoredContent` を返す。共通のID・金額・商品情報の変換は既存の処理を `types.rs` で再利用し、SQLiteのRepositoryには依存しない。 commandsは増減額・ギルドID・商品ID・購入ID・ロールIDをDiscord Repositoryのメソッドで変換し、共通の変換関数を直接呼ばない。ページ選択やボタンの振分けはDiscordの対話処理に置く。

SQLiteの `NotRepository`・`ShopRepository` はドメイン値を受け取り、保存値の変換、残高・商品版・履歴の照合、追記と結果の復元を行う。購入結果は `ShopRepository::purchase_result`、返金結果は `refund_result` で元取引・ギルド・本人・金額・売上の有無を照合してから生成する。新規処理のCOMMIT前と履歴復元で同じ検証を使う。

Discordへの返信、Embed、購入確認、ロール操作は `commands.rs`、投稿・リアクション・通話などのGateway処理は `events.rs` に置く。Poiseの `Data` はRepositoryと下書き・確認・公開ロック・通話の参加状態を保持する。商品公開記録の読出しと追記はSQLite ShopRepositoryを使い、Discord APIを待つ間はDB取引を保持しない。 Dataの参照名は `sqlite_not`・`sqlite_shop`・`discord_shop` とし、呼び出すRepositoryを区別する。商品変更・削除・購入一覧はコマンド関数からSQLite Repositoryを直接呼ぶ。

独立したアプリケーション層、Adapter層、RepositoryのTrait、同じ内容のDTOやMapperは追加しない。同名のRepositoryは `discord::not`・`discord::shop` と `sqlite::not`・`sqlite::shop` のパスで区別する。Poise・Serenity・SeaORMの既存機能を使う。

すべての `mod.rs` はモジュール宣言と再公開だけにする。エラーは基本的に `?` で返し、Discordの入口で返信する。返金・再投稿など動作が変わる失敗だけ分岐する。SDKエラーの秘密情報は返信やログに出さない。

テストは `tests/discord/` と `tests/sqlite/` にまとめる。値変換・結果検証は `sqlite/values.rs`、実SQLiteの取引は `sqlite/flows.rs`。非公開処理のテストは実装側から `#[path]` で読み込み、テスト用の公開APIは追加しない。テスト名は日本語で条件と期待する振る舞いを表す。

業務仕様は [CONTENT.md](CONTENT.md)、保存構造は [DATABASE.md](DATABASE.md)、操作とシーケンスは [REPOSITORY.md](REPOSITORY.md)、進捗は [IMPLEMENTATION.md](IMPLEMENTATION.md)を参照する。
