# NotBot

Nifty Online Tokensを管理するDiscord Botです。メッセージ・リアクションによるNOTの増減、ギルド内の残高・ランキング、Discord内で完結するコンテンツ売買を予定しています。

P06までのDB・ユースケースに加え、P07のDiscord接続と残高コマンドを実装しています。P07は自動検証済み・実機確認待ちです。コンテンツの永続保存・商品操作・購入・返金はユースケースまで実装済みで、Discordへの接続はP08以降です。

## 起動

Rust 1.94以降を用意し、設定例をコピーしてBotトークンを入力します。

```sh
cp .env.example .env
# .env の DISCORD_TOKEN に実際のBotトークンを設定
set -a
. ./.env
set +a
cargo run --locked
```

起動時に設定・SQLiteを初期化し、`/not` をグローバル登録してDiscordへ接続します。同名のコマンドを更新し、他のコマンドは置換しません。Ctrl-CまたはSIGTERM（Unix）でGatewayを終了し、DBを閉じます。トークンやDiscordのエラー本文はログへ出しません。

環境変数の `DATABASE_PATH` と `CONTENT_DIR` は省略すると `data/notbot.sqlite` と `data/content` を使います。`SHOP_CHANNELS` は `ギルドID=チャンネルID` をカンマ区切りで指定します。Discord IDは10進数の文字列です。チャンネル対応は省略でき、P07の残高コマンドには不要です。

DBファイルと親ディレクトリは自動作成します。7テーブル・3VIEW・取引理由の初期値は同じトランザクションで初期化し、再起動時はSQLiteの `user_version` で適用済みかを判定します。DBは実ファイルを使い、`:memory:` は受け付けません。WALモードで動かし、SQLiteのロック待機時間は5秒に設定しています。ロック競合は `Conflict` として返します。

```dotenv
DISCORD_TOKEN=
DATABASE_PATH=data/notbot.sqlite
CONTENT_DIR=data/content
SHOP_CHANNELS=123456789012345678=234567890123456789
```

テストはBotトークンなしで実行できます。

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## Discordの設定と残高コマンド

1. [Developer Portal](https://discord.com/developers/applications)でテスト用アプリケーションとBotを作り、Botトークンを `.env` に設定します。
2. InstallationでGuild Installを有効にし、`bot`・`applications.commands` スコープでテスト用サーバーA・Bへ招待します。Botへ管理者権限は不要です。テスト用テキストチャンネルで「チャンネルを見る」を許可し、利用者には「アプリコマンドを使う」を許可してください。P07はInteractionへの返信だけを使い、通常投稿・添付・ロール管理のBot権限はまだ使いません。IDを直接指定する場合はDiscordの「詳細設定 → 開発者モード」を有効にし、各サーバーの「サーバーIDをコピー」を使います。
3. Privileged Gateway Intentsはすべて無効のままにします。P07は `GUILDS` だけを要求し、ギルド名の候補表示にSerenityのキャッシュを使います。所属は単一メンバーのREST取得で確認し、メンバー一覧取得・メッセージ本文の受信は行いません。[Gateway Intent](https://docs.discord.com/developers/events/gateway#gateway-intents)、[メンバー取得](https://docs.discord.com/developers/resources/guild#get-guild-member)
4. `cargo run --locked` で起動します。WebhookではなくGatewayで受信するため、Interactions Endpoint URLは設定しません。グローバルコマンドはギルドとBotのDMコンテキストを有効にしています。反映後にBotのプロフィールからDMを開いてください。[コマンドの登録・コンテキスト](https://docs.discord.com/developers/interactions/application-commands)

```text
# サーバー内（guild省略で現在のサーバー）
/not balance
/not ranking
/not adjust user:@対象ユーザー balance:1200

# BotとのDM（guild必須、候補の選択またはサーバーIDの直接入力）
/not balance guild:123456789012345678
/not ranking guild:123456789012345678
/not adjust user:@対象ユーザー balance:1200 guild:123456789012345678
```

`balance` は本人の残高、`ranking` は履歴のある上位10人を表示します。返信は実行者だけに表示し、ユーザーへの通知メンションは発生させません。サーバー内から別サーバーへの操作は拒否します。

`adjust` は対象サーバーの所有者または `ADMINISTRATOR` を持つメンバーだけが使えます。`MANAGE_GUILD` だけでは許可しません。共通の `/not` を一般ユーザーも使うため、調整サブコマンドは表示されても実行時に認可します。対象ユーザーにも同じサーバーへの所属を要求します。目標残高は0〜9223372036854775807の整数文字列です。DiscordのINTEGERの上限を避け、丸めず検証します。

DM候補にはBotと本人の所属を確認できたサーバーだけを表示し、調整の候補には管理権限も要求します。候補表示は2秒以内に確認できた最大25件です。多数のサーバーやAPIの混雑で候補が出ない場合は、名前の一部・IDで絞り込むかIDを直接入力してください。直接入力でも、実行時にBotの参加・本人の所属・最新ロールを再確認します。

実機確認には、管理者・一般ユーザーの2アカウントと2サーバーを使います。現在は未実施です。

- サーバーAの管理者が一般ユーザーを1200 NOTへ調整し、本人がサーバー内とDMの両方で1200 NOTとランキングを確認する。
- 同じ本人がサーバーBでは0 NOTを確認し、Aの履歴がランキングへ混ざらないことを確認する。
- 一般ユーザーの調整、DMでのguild省略、所属しないギルドID、サーバー内からの別ギルド指定を拒否することを確認する。
- DM候補に所属外・Bot未参加のサーバーが出ず、調整候補には管理可能なサーバーだけが出ることを確認する。権限剥奪・退会後の実行も拒否されることを確認する。
- Ctrl-C／SIGTERMで終了し、再起動後も残高が保持されることを確認する。

参考の[JadeMusicの起動処理](https://github.com/segfau-yama/JadeMusic/blob/main/src/main.rs)でSerenityクライアントとグローバル登録の流れを確認しました。同じPoiseフレームワークを採用し、Discord側を `commands.rs`・`events.rs`・`services.rs` に分けています。コマンドの定義・引数解析・返信をPoiseに任せ、所属・権限のREST確認にはSerenityを使います。

## 残高操作

`infrastructure::sqlite::not::NotUseCase::new(database)` に `SqliteDatabaseAccess` を渡して使います。

- `get_balance(actor)`：実行者の対象ギルドでの残高。履歴がなければ0。
- `get_ranking(actor, limit)`：対象ギルドの残高降順。同額は保存されたユーザーID文字列の昇順。履歴のないユーザーは含めず、件数0は空の一覧。
- `adjust_balance(actor, user_id, target_balance, request_id)`：管理者が0以上の目標残高を指定し、現在との差額だけを追記。差額0も記録します。

調整の再試行では同じ依頼IDを使います。同じギルド・対象ユーザー・目標残高なら元の取引を返し、追加反映しません。対象や目標を変えて同じIDを使うと `RequestConflict` になります。後から別の取引があっても、過去の依頼の再試行で現在残高を戻すことはありません。

`Actor` は呼び出し側が認証済みの所属・権限から生成する契約です。`infrastructure::discord` がDiscordのREST照会結果から生成し、上記コマンドをユースケースへ接続します。

## コンテンツと商品操作

`infrastructure::discord::content::store_content` に保存先、認証済みの `Actor`、Botが発行した出品操作ID、元ファイル名、`Read` を渡して保存します。戻り値の `StoredContent` と `ItemDetails` を `infrastructure::sqlite::shop::ShopUseCase::create` に渡すと、ファイル情報・商品・初版を同じトランザクションで確定します。未保存の `ContentFile` だけから商品を作る入口はありません。

1商品1ファイル、正のサイズから20 MiBまでを受け付けます。Discordの既定のファイル上限に合わせ、25 MiBの送信リクエスト上限内にメタデータ用の余地を残します。元ファイル名はUTF-8で255バイト以内とし、保存パスには使いません。[Discordのファイル仕様](https://docs.discord.com/developers/reference#uploading-files)、[送信リクエスト上限](https://docs.discord.com/developers/resources/message#create-message)

保存先はBot専用の非公開な永続ディスクを指定してください。キーは `content-{guild_id}-{user_id}-{request_id}` で、操作IDは1〜128文字のASCII英数字・ハイフン・アンダースコアに限定します。同じ下書きの再試行ではIDを変えません。同じキーに別の内容が来た場合や、既存の保存先がシンボリックリンクの場合は拒否します。同期I/Oなので、非同期のイベント処理からは `spawn_blocking` で呼び、DBセッションを保持したまま呼ばないでください。

`ShopUseCase::find` と `list` は対象ギルドの販売中の商品を返します。`update(actor, item_id, expected_revision, details)` と `delete(actor, item_id, expected_revision)` は出品者本人だけが実行でき、履歴の新版をINSERTします。出品者・ギルド・ファイルは変えません。作成の再試行は初版の内容と照合して現在の商品を返し、変更の再試行は期待する次の版と同じ内容ならその版を返します。異なる内容や版の競合は拒否します。削除済みへの新しい変更は拒否し、削除の再試行は成功として扱います。

商品削除やDB確定エラーでも完成ファイルは保持します。DB結果が不明な場合も同じ操作IDで再試行し、ファイルを消さないでください。確定されなかった完成ファイルや、プロセス強制終了時の `.pending-*` の自動削除は行いません。実Discordからの添付取得・公開・DM配送は後続工程で接続します。

## 購入確認と決済

`ShopUseCase::prepare_purchase(actor, item_id, request_id, now)` は、本人・ギルドに結び付いた5分間の `PurchaseQuote` を返します。`item()` に確認時の商品版と価格を保持し、確認発行では課金しません。確認IDはBot側で発行し、同じ確認の再試行では変更しません。

`purchase(actor, &quote, now)` は、同じ確認IDで確定済みかを先に照合します。初回だけ期限、最新の版・価格、販売状態、自己購入・購入済み、両者の残高を検証し、購入者の減算と出品者の加算を同じImmediateトランザクションで確定します。例として1,200 NOTで500 NOTの商品を買うと、購入者700・出品者500になります。価格を元に戻しても版が変わっていれば再確認が必要です。

`now` はコマンドやボタンの受付時に呼び出し側が `SystemTime::now()` で採取したサーバー時刻を渡し、利用者が指定した時刻を使いません。期限ちょうどで未処理の確認は失効します。確定済みなら期限・商品変更・削除・後続の残高変更で再課金せず、元の取引を返します。確認IDが同じでも購入者・商品・金額が異なれば `RequestConflict`、履歴が片側しかなければ `InvalidData` になります。

`PurchaseResult` の `buyer()`・`seller()` は確定した取引、`item()` は現在の商品状態です。実際の支払額には `buyer().amount()` を使い、現在の商品価格から計算し直しません。この結果だけで配送を許可せず、以下の購入権限照会を使います。ファイル確認・DM配送はP09で接続します。

## 購入結果の復元・再送権限・返金

- `find_purchase_by_confirmation(actor, confirmation_id)`：本人・対象ギルドの領収結果を復元します。未処理なら `None`。返金済みでも元の取引を返し、課金行を増やしません。
- `list_purchases(actor)`：本人の未返金購入を購入IDの降順で返します。既存の `PurchaseResult` を使い、商品変更・削除後も一覧に含めます。
- `get_content_for_purchase(actor, purchase_id)`：購入者・ギルドと購入／売上／返金の整合性を照合し、配送用の `ContentFile` を返します。購入IDには `PurchaseResult::buyer().id()` を使います。返金済み・別人には取得を許可しません。
- `refund(actor, purchase_id, request_id)`：NOT管理権限を持つ実行者だけが、元の購入額で購入者への返金と出品者の売上取消を一括追記します。戻り値 `TradeResult` の `buyer()`・`seller()` が返金の2行です。

返金キーは `refund:{request_id}:buyer`・`refund:{request_id}:seller`。同じ依頼ID・購入の再試行は、残高が後から変わっても元の結果を返します。同じIDを別購入に使うと `RequestConflict`、別IDによる二重返金は `AlreadyRefunded` です。出品者の残高不足・購入者の残高上限超過・途中のSQL失敗では、返金の両行とも残しません。

返金後も領収結果の復元はできますが、配送権限は失われます。ファイル自体は削除しません。配送の直前に権限を確認し、返金や配送失敗を理由に同じ確認IDで購入をやり直さないでください。Discordでの実送信はまだ実装していません。

## 依存ライブラリ

`Cargo.toml` では主要ライブラリのバージョンを固定し、`Cargo.lock` で依存全体を固定しています。

| 用途 | ライブラリ | バージョン |
|---|---|---|
| 非同期ランタイム | Tokio | 1.53.3 |
| Discordコマンド | Poise | 0.7.1 |
| Discord API | Serenity | 0.12.5 |
| ORM・SQLite | SeaORM | 2.0.4 |
| エラー表示・変換の生成 | thiserror | 2.0.21 |

起動・Discord処理は[JadeMusicの起動処理](https://github.com/segfau-yama/JadeMusic/blob/main/src/main.rs)を参考に、共通の `Box<dyn Error + Send + Sync>` と `?` で失敗を返します。定型的な `Display`・ラップするだけの `From` は [thiserror](https://github.com/dtolnay/thiserror)に任せ、返信失敗はイベント入口でまとめて扱います。thiserrorは既存の間接依存を直接依存として宣言したもので、依存パッケージの追加・バージョン変更はありません。

## 設計と実装手順

コードもドメイン層（共通型・値の不変条件）とインフラ層（変換・検証・操作・I/O）の2層へ整理しています。共通型は `domain::`、操作・DB・Discord・ファイル処理は `infrastructure::` を使います。旧 `application::` とDB向けTrait・型引数は削除しました。インフラ内は `sqlite/` にDBと操作処理、`discord/` にPoiseのコマンド・イベント・認証・ファイル保存をまとめています。配置は[アーキテクチャ設計](design/ARCHITECTURE.md)を参照してください。

- [実装手順とCodexへの依頼文](design/IMPLEMENTATION.md)
- [コンテンツ売買のレビューと操作フロー](design/CONTENT.md)
- [アーキテクチャ設計](design/ARCHITECTURE.md)
- [DB設計](design/DATABASE.md)
- [インフラの操作処理とDBアクセス](design/REPOSITORY.md)
