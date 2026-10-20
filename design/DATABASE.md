# データベース設計

NOTの増減・商品・取引理由を、すべてINSERTで保存するSQLite設計。既存行へのUPDATE・DELETE・REPLACEは使わない。商品の更新・削除と理由の表示名変更は履歴の追記で表し、現在の状態はVIEWで取得する。Discordのユーザー情報は保存しない。コンテンツは[Discord内の購入フロー](CONTENT.md)で売買する。

## テーブルと参照

固定の識別情報・変更履歴・添付ファイル・公開投稿・報酬の処理結果を分けた8テーブルと、照会用の3つのVIEWを使う。

実装SQLは [001_initial.sql](../src/infrastructure/sqlite/001_initial.sql) と [002_rewards.sql](../src/infrastructure/sqlite/002_rewards.sql)。起動時に `SqliteDatabaseAccess::open` が適用する。現在のSQLiteの `user_version` は2。新規DBは両SQLと版番号を同じImmediateトランザクションで確定し、既存の版1には報酬の処理結果と索引だけを追加する。ファイルヘッダの版管理であり、業務テーブルの既存行は書き換えない。[SQLiteのuser_version](https://www.sqlite.org/pragma.html#pragma_user_version)

```mermaid
erDiagram
    transaction_reason ||--|{ transaction_reason_revision : "表示名の履歴"
    transaction_reason ||..o{ not_transaction : "reason"
    shop_item ||--|{ shop_item_revision : "商品情報の履歴"
    shop_item_revision ||..o{ shop_item_publication : "公開した版"
    content_file |o..o{ shop_item : "CONTENTのtargetをアプリで照合"
    shop_item |o..o{ not_transaction : "item_id"
    not_transaction |o..o| not_transaction : "sale_of"
    not_transaction |o..o| not_transaction : "refund_of"
    transaction_reason ||..o{ reward_event : "reason"
    not_transaction |o..o| reward_event : "付与結果のentry_id"

    transaction_reason {
        TEXT code PK "固定の取引理由コード"
    }
    transaction_reason_revision {
        TEXT code PK, FK "取引理由コード"
        INTEGER revision PK "1から始まる版番号"
        TEXT name "この版の表示名"
        TEXT created_at "記録日時"
    }
    content_file {
        TEXT key PK "Bot生成の保存キー"
        TEXT filename "配送時の元ファイル名"
        INTEGER size_bytes "ファイルサイズ"
        TEXT created_at "保存日時"
    }
    shop_item {
        INTEGER id PK "固定の商品ID"
        TEXT guild_id "DiscordサーバーID"
        TEXT seller_id "固定の出品者ID"
        TEXT kind "固定のROLEまたはCONTENT"
        TEXT target "固定のロールIDまたはコンテンツ識別子"
    }
    shop_item_revision {
        INTEGER item_id PK, FK "商品ID"
        INTEGER revision PK "1から始まる版番号"
        TEXT name "この版の商品名"
        TEXT description "この版の商品説明"
        INTEGER price "この版の価格（正のNOT整数）"
        TEXT deleted_at "削除日時（販売中はNULL）"
        TEXT created_at "記録日時"
    }
    shop_item_publication {
        INTEGER id PK "公開記録ID"
        INTEGER item_id FK "商品ID（版番号との複合FK）"
        INTEGER item_revision FK "公開した商品版"
        TEXT channel_id "販売チャンネルID"
        TEXT message_id "公開メッセージID"
        TEXT created_at "記録日時"
    }
    not_transaction {
        INTEGER id PK "取引ID"
        TEXT guild_id "DiscordサーバーID"
        TEXT user_id "DiscordユーザーID"
        INTEGER amount "増減額（加算は正、減算は負）"
        TEXT reason FK "transaction_reason.code"
        TEXT event_key "サーバー内で一意な重複処理防止キー"
        INTEGER item_id FK "購入商品（PURCHASEのみ）"
        INTEGER sale_of FK, UK "対応する購入（SALEのみ）"
        INTEGER refund_of FK, UK "取消元の購入または売上"
        TEXT created_at "記録日時"
    }
    reward_event {
        TEXT guild_id PK "DiscordサーバーID"
        TEXT event_key PK "報酬種別と元イベントのキー"
        TEXT user_id "受取人"
        TEXT reason FK "報酬の4種別"
        INTEGER received_at "Bot受付日時のUnixミリ秒"
        INTEGER entry_id FK, UK "付与した取引ID。制限で見送った場合はNULL"
    }
```

VIEWは `current_transaction_reason`（理由の最新表示名）、`current_shop_item`（削除済みを含む商品の最新状態）、`not_balance`（サーバー・ユーザーごとの残高）。VIEWから履歴テーブルへの書き戻しは行わない。

`not_transaction.id` は追記APIがSQLiteの自動採番を使い、追記順に増加させる。残高調整の再試行では、このIDまでの履歴合計で当時の目標残高を照合する。既存行の削除や任意IDでの挿入は通常操作に含めない。最大IDが `i64::MAX` に到達した後は、SQLiteが小さいIDをランダムに採番する動作を避けるため、追記APIがINSERTを拒否する。テーブル定義の変更や `AUTOINCREMENT` の追加はしない。[SQLiteのROWID採番](https://www.sqlite.org/autoinc.html)

## SQLと現在状態

DDL・CHECK・索引・初期値・VIEWの定義は [001_initial.sql](../src/infrastructure/sqlite/001_initial.sql)、報酬の追加は [002_rewards.sql](../src/infrastructure/sqlite/002_rewards.sql)を正とする。設計書へ同じSQLを複製しない。

`reason` は固定のマスタコードを参照し、存在しない理由をFKで拒否する。新しい理由には符号・参照列のCHECK制約と操作処理も対応させる。理由名の変更は履歴へ追記する。

最新版は日時ではなく識別子ごとの最大 `revision` で決める。各版は差分ではなくその版の完全な状態を持つ。残高VIEWは履歴の `SUM(amount)` を実行し、履歴なしは0とする。残高不足や出品者本人かの判定は、同じDB取引内の操作処理で行う。

## 追記の単位と業務上の制約

- 商品作成では `shop_item` と初版 `shop_item_revision` を同一トランザクションでINSERTする。理由の新規登録も固定コードと初版をまとめて記録する。初版のない識別行は確定しない。
- 商品名・説明・価格の変更は最新状態を取得し、版番号を1増やした行をINSERTする。削除は名前・説明・価格を引き継いで `deleted_at` を設定した新版をINSERTする。削除済みの商品は再変更せず、再度の削除は成功として扱う。
- 商品のサーバー・出品者・種別・取得先は初版から固定する。変更が必要なら別商品として登録する。同じサーバー・種別・取得先は削除後も同じ商品に対応し、再登録しない。取得先のコンテンツは既存購入者のため保持する。
- 理由の表示名変更は `transaction_reason_revision` に次の版を追記する。履歴表示は現在の表示名を使う。取引時点の表示名の固定は要件に含めない。
- 商品・理由の版追加は `BEGIN IMMEDIATE` 内で最新版を読み、次の版をINSERTする。版番号の複合主キーで重複を拒否する。商品変更では認証済みの出品者、理由変更では運営権限を確認する。
- 購入は販売中の商品・同じサーバー・コンテンツの自己購入でないこと・未購入を検証する。コンテンツは購入者の `PURCHASE`（負数、`item_id`）と出品者の `SALE`（同額の正数、`sale_of`）を一括INSERTし、合計0とする。ロールは `PURCHASE` だけをINSERTし、出品者に売上を付けない。出品者本人も価格以上の残高があれば購入でき、代金分を減算する。
- 返金は元の購入額の符号を反転した `REFUND` を追記する。元の売上がある場合だけ `SALE_REFUND` も一括追記し、出品者の残高不足なら全体を取り消す。売上なしのロール購入は出品者の残高を変更しない。`refund_of` は元の取引を参照し、現在価格は使わない。旧仕様の売上付きロール購入も元の履歴で復元・返金する。本人の旧購入の相殺で整数上限を超えないよう、売上取消がある場合は負数行を先に記録する。
- `sale_of` は同じサーバーの `PURCHASE`、`REFUND.refund_of` は同じ購入者の `PURCHASE`、`SALE_REFUND.refund_of` は同じ出品者の `SALE` を参照する。商品のサーバー・出品者と金額の一致もユースケースが同一トランザクションで検証する。FKだけではこれらの一致までは保証しない。
- `sale_of`・`refund_of` の一意制約で二重売上・二重取消を防ぐ。購入権限は返金されていない `PURCHASE` から判定し、削除済み商品でも既存の権限・返金・ロール照合に使う。
- 罰則は残高を下限0まで減らす。差額0の調整や残高0での罰則も0の取引として記録し、同じイベントの再処理を防ぐ。
- `event_key` は操作種別・依頼IDを含める。譲渡は `:sent`・`:received`、購入・返金は `:buyer`・`:seller` で区別し、再試行時も同じキーを使う。売上なしのロール購入・返金には `:buyer` だけを記録する。

すべての残高変更は、`BEGIN IMMEDIATE` → 重複確認 → VIEWから残高取得 → 残高不足・オーバーフロー等の検証 → INSERT → COMMITの順とする。失敗時は全体をROLLBACKする。2つのINSERTを別々に確定すると片側だけの取引が残り得る。残高100を別々に確認した2件が各80を引けば、VIEWの結果は−60になるため、集計の採用後も同じトランザクション内での検証が必要。[SQLiteのトランザクション仕様](https://www.sqlite.org/lang_transaction.html)

<!-- ponytail: SQLiteの単一ライタで書き込みを直列化する。競合が実測で問題になったら、別DBで同じ整合性契約を実装する。 -->

## 報酬の処理結果

`reward_event` は付与と制限による見送りの両方をINSERTで記録する。`entry_id` がNULLなら見送り、付与なら同じトランザクションで記録した報酬の取引を参照する。報酬額は投稿5・リアクション実行1・リアクション受信10・返信受信20 NOT。旧仕様で付与済みの1 NOTは履歴を変更せず、再試行でも元の額を返す。返信・リアクションの各受取人は独立して判定する。

`received_at` はBotの受付時刻をUnixミリ秒で保存する。サーバー・受取人・報酬種別ごとに付与済み結果の最新時刻と当日件数を読み、60秒間隔と日本時間00:00からの100回上限を照合する。UTCのUnixミリ秒へ9時間を加えて日付を区切り、日付変更でも間隔は維持する。見送り結果は制限の件数や最終付与時刻へ含めず、同じイベントの再試行では元の見送りを返す。

報酬の取引と処理結果を同じImmediateトランザクションで確定する。DB移行もDDLと版番号を一括確定し、いずれもDBセッション中にDiscord・ファイルI/Oを行わない。処理結果だけ・付与だけの片側確定は許可しない。既存テーブルに0 NOTの報酬を混ぜず、取引理由の正数制約を保持する。

通話報酬は既存の `not_transaction` に `reason='ADJUSTMENT'`・`amount=1`・`event_key='voice:{日本時間の日番号}:{ユーザーID}:{Unix分}'` で追記する。`reward_event` の4種別には混ぜず、スキーマ版2を維持する。同じギルド・キーの元結果を先に復元し、未処理なら当日の同じユーザーの `voice:` 履歴を数えて300回未満の場合だけ付与する。残高検証と付与も同じImmediateトランザクションで確定する。既存の `(guild_id, user_id)` 索引を使い、チャンネル移動・退出・再起動で付与数を失わない。上限後の見送り行は作らず、当日の履歴が残る限り付与しない。

## コンテンツと公開投稿

- `CONTENT` の `target` は `content_file.key` に対応する。`ROLE` ではロールIDを使うため、この対応はFKではなくアプリケーションが検証する。コンテンツ作成時は、保存済みファイル情報・商品・初版を同じトランザクションでINSERTする。未保存のキーを商品にしない。
- P13のROLE商品は既存の `shop_item` と初版を同じトランザクションでINSERTし、`content_file` は作らない。購入・返金は既存の履歴と制約を共用し、購入キーの `content:` 接頭辞も維持する。売上行は任意の参照なので、作成を止めてもスキーマ変更は不要。起動時のロール照合は未返金の購入履歴から復元し、配送状態テーブルは追加しない。
- ファイル本体は非公開の永続ディスクに置く。キー・元のファイル名・サイズは不変とし、差し替えは別の商品として登録する。元のDiscord添付URLは保存先にしない。
- 商品説明は商品版に含める。購入確認は商品版と価格を固定し、確定時に最新版と一致しなければ再確認する。`content:{quote_id}:buyer`・`content:{quote_id}:seller` を取引の重複処理防止キーにする。
- `shop_item_publication` はDiscordへ投稿・編集できた商品版とメッセージを追記する。編集では同じメッセージIDに新しい商品版を記録でき、削除されたメッセージの再投稿では新しいメッセージIDを記録する。最新の公開先は商品ごとの最大 `id` から取得する。
- 商品確定と公開投稿は別処理。投稿失敗なら既存の商品IDで再投稿する。Discordが投稿を受理した直後に記録が失敗すると、再投稿で表示が重複する場合がある。すべてのボタンを同じ商品IDへ結び付け、課金の整合性はDBで保証する。公開投稿のための分散トランザクションや専用キューは作らない。
- ファイル配送の成功を購入権限の根拠にしない。購入済みかつ未返金の履歴を根拠に再送できるようにし、配送失敗で購入・売上を自動取消ししない。取引・配送の結果不明時は既存の購入IDを照合する。

## 正規化の確認

固定コード・固定の商品情報と、版番号に依存する表示名・商品名・説明・価格を別表にした。履歴のキーは `(code, revision)`・`(item_id, revision)` で、最新状態はVIEWで導出する。残高も `amount` の合計だけから導出し、別テーブルへ複製しない。各商品版が同じ名前・価格を持つことは、各時点の状態を記録するための履歴である。

取引理由名を取引行へ、商品IDを売上・返金行へ複製しない。一方、参照元から求められるサーバーIDやユーザーIDは、残高照会のため各増減行に意図的に持つ。この重複は同一トランザクションでの一致検証を必要とするため、全体を厳密な第3正規形と断定しない。

ファイル情報は保存キー、公開投稿は公開記録IDに依存する。販売用Embedに必要な情報は商品・版・出品者から組み立て、Embed全体をDBへ複製しない。公開投稿のレコードは公開した版を参照し、ファイル名・商品名・価格は複製しない。
