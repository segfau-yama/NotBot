# 構成

ドメインとインフラの2層とする。ドメインは検証済みの値・商品・取引・結果を持ち、インフラはDiscord入力の変換・認証・操作・DBとファイルのI/Oを行う。独立したアプリケーション層はない。

```text
src/
  domain.rs
  infrastructure/
    mod.rs          # モジュール宣言・再公開
    config.rs
    types.rs        # 共通エラー・保存済みファイルの型
    discord/
      mod.rs        # モジュール宣言・再公開
      bot.rs        # Poiseの起動・共有データ・終了
      commands.rs   # スラッシュコマンド
      events.rs     # Gateway・ボタンの受付、投稿・通話報酬
      services.rs   # 認証、出品・公開・購入・配送・ロール操作
      content.rs    # 添付取得・永続ファイルの保存と読出し
    sqlite/
      mod.rs        # モジュール宣言・再公開
      database.rs   # 接続・移行・取引境界・SQL・DBエラー
      not.rs        # 残高・調整・報酬・譲渡
      shop.rs       # 商品・購入・返金
      001_initial.sql
      002_rewards.sql
  lib.rs
  main.rs
```

Poiseのコマンド定義・引数解析とSerenityのイベント・モデル・権限計算を使う。DBはSeaORMとSQLiteの具体型を直接呼ぶ。共通値のために同じ内容のDTOやMapperは作らない。

すべての `mod.rs` はモジュール宣言と再公開だけにし、構造体・列挙型・型別名・定数・関数・`impl` は役割に合う実装ファイルへ置く。

値単体の不変条件はドメインのコンストラクタで保証する。所属・権限はDiscord側で、残高・商品版・購入済みなどの状態はDB取引内で確認する。ドメインからSDK・ORMは参照しない。

エラーは基本的に `?` で返し、Discordの入口で返信する。返金・再投稿など動作が変わる失敗だけ分岐する。秘密トークンを含み得るSDKエラーは返信やログにそのまま出さない。

関数はコマンド・イベント・業務操作の入口と、実際に共有する処理を中心にする。一度しか使わない単純な処理のために補助関数やファイルを増やさない。

テストはすべて `tests/` に置く。`domain.rs`・`sqlite_flows.rs` は結合テスト、`discord/` は単体テストと共通のローカルHTTP基盤。非公開処理の単体テストは製品モジュールから `#[path]` で読み込み、テストのためにAPIを公開しない。

業務仕様は [CONTENT.md](CONTENT.md)、保存構造は [DATABASE.md](DATABASE.md)、操作の契約は [REPOSITORY.md](REPOSITORY.md)、進捗と検証は [IMPLEMENTATION.md](IMPLEMENTATION.md)を参照する。
