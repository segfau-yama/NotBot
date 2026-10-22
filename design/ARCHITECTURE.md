# 構成

設計方針は [DESIGN.md](DESIGN.md) を参照する。domainは業務データの不変条件・金額計算・購入と返金の整合性を持つ。フィールドをprivateにし、検証する生成メソッドと読み取り操作を公開する。Discord・DBの形式変換とI/Oはインフラに置き、domainはSDK・ORM・具体的なRepositoryを参照しない。

```text
src/
  domain.rs              # 業務データ・不変条件・金額計算・結果の整合性
  infrastructure/
    mod.rs                # モジュール宣言・再公開
    config.rs
    types.rs              # 外部表記の解析・永続化のエラー・domainのエラーの再公開
    discord/
      mod.rs              # モジュール宣言・再公開
      bot.rs              # Poiseの起動・共有状態とRepositoryの組み立て・終了
      commands.rs         # コマンド・出品と購入の対話・送信・ロール操作
      events.rs           # Gateway・ボタンの受付・投稿と通話の報酬
      not.rs              # NotRepository: Discordから所属・権限を取得しActorへ変換
      shop.rs             # ShopRepository: CRUD要求をdomainの型へ変換
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

Discordの `NotRepository` は状態を持たず、API取得を行うメソッドの引数にSerenityの `&Http` を受け取り、本人・所属・権限から `Actor` を生成する。`ShopRepository` は商品入力を `ItemDetails` へ変換し、保存先にファイルを保存して `StoredContent` を返す。IDの文字列解析は `types.rs` で共用し、値の不変条件はdomainの生成メソッドで検証する。SQLiteのRepositoryには依存しない。 commandsは増減額・ギルドID・商品ID・購入ID・ロールIDをDiscord Repositoryのメソッドで変換し、共通の変換関数を直接呼ばない。ページ選択やボタンの振分けはDiscordの対話処理に置く。

SQLiteの `NotRepository`・`ShopRepository` はドメイン値を受け取り、保存値の変換、残高・商品版・履歴の照合、追記と結果の復元を行う。購入結果は `PurchaseResult::new`、返金結果は `RefundResult::new` が元取引・ギルド・本人・金額・売上の有無を照合する。新規処理のCOMMIT前と履歴復元で同じ生成経路を使い、復元時の違反はSQLite側で `DatabaseError::InvalidData` に変換する。

Discordへの返信、Embed、購入確認、ロール操作は `commands.rs`、投稿・リアクション・通話などのGateway処理は `events.rs` に置く。Poiseの `Data` はRepositoryと下書き・確認・公開ロック・通話の参加状態を保持する。商品公開記録の読出しと追記はSQLite ShopRepositoryを使い、Discord APIを待つ間はDB取引を保持しない。 Dataの参照名は `sqlite_not`・`sqlite_shop`・`discord_not`・`discord_shop` とし、起動時に各Repositoryを組み立てる。Discordの入力変換・認証・添付取得もDataのインスタンスから呼び、コマンドやイベント内ではRepositoryを生成しない。商品変更・削除・購入一覧はコマンド関数からSQLite Repositoryを直接呼ぶ。

中継するだけの層・struct・DTOは追加しない。業務処理から永続化を呼ぶ境界が必要になった場合だけ、内側に必要最小限のtraitを置く。同名のRepositoryは `discord::not`・`discord::shop` と `sqlite::not`・`sqlite::shop` のパスで区別する。Poise・Serenity・SeaORMの既存機能を使う。

すべての `mod.rs` はモジュール宣言と再公開だけにする。エラーは基本的に `?` で返し、Discordの入口で返信する。返金・再投稿など動作が変わる失敗だけ分岐する。SDKエラーの秘密情報は返信やログに出さない。

テストは `tests/discord/` と `tests/sqlite/` にまとめる。値変換・結果検証は `sqlite/values.rs`、実SQLiteの取引は `sqlite/flows.rs`。非公開処理のテストは実装側から `#[path]` で読み込み、テスト用の公開APIは追加しない。テスト名は日本語で条件と期待する振る舞いを表す。

業務仕様は [CONTENT.md](CONTENT.md)、保存構造は [DATABASE.md](DATABASE.md)、操作とシーケンスは [REPOSITORY.md](REPOSITORY.md)、進捗は [IMPLEMENTATION.md](IMPLEMENTATION.md)を参照する。
