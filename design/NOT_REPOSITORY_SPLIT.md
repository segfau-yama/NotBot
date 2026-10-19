# NotRepositoryの分割方針

採用する構成は[ユースケースと永続化の設計](REPOSITORY.md)にまとめた。ブロック図とクラス図も同書を正とする。

前案の `NotRepository` と `PurchaseRepository` への分割から、業務操作をユースケースへ、SQLとトランザクションの実行をSQLiteアダプターへ分ける設計へ更新した。Bot・Webは共通のユースケースを呼び、業務ルールはDBなしで検証できる。

## 操作の移動先

引数・正常時の戻り値は従来の設計を引き継ぐ。

| 旧API | 新しい呼び出し先 |
|---|---|
| `NotRepository.get_balance` | `NotUseCases.get_balance` |
| `NotRepository.get_ranking` | `NotUseCases.get_ranking` |
| `NotRepository.list_history` | `NotUseCases.list_history` |
| `NotRepository.grant_reward` | `NotUseCases.grant_reward` |
| `NotRepository.penalize` | `NotUseCases.penalize` |
| `NotRepository.adjust_balance` | `NotUseCases.adjust_balance` |
| `NotRepository.transfer` | `NotUseCases.transfer` |
| `NotRepository.purchase` | `PurchaseUseCases.purchase` |
| `NotRepository.refund` | `PurchaseUseCases.refund` |
| `NotRepository.has_purchase` | `PurchaseUseCases.has_purchase` |
| `NotRepository.list_role_purchases` | `PurchaseUseCases.list_role_purchases` |
| `ShopItemRepository.find`・`list`・`create`・`update`・`delete` | `ShopItemUseCases` の同名操作 |

`PurchaseRepository` は新設しない。永続化の契約は `application::ports` の `Store`・`WriteTransaction` の2つとし、`SqliteStore`・`SqliteWriteTransaction` が実装する。テーブルごとの汎用リポジトリは追加しない。

## 実装する順序

1. `domain` に独自の業務データ型と純粋なルール関数を実装し、DBなしで検証する。
2. `application` にユースケースと永続化の契約を実装する。購入・返金の状態取得から検証・2行の保存までを、1回の `Store.write` に含める。
3. SQLiteアダプターを実装し、`BEGIN IMMEDIATE`・COMMIT・ROLLBACKと全接続での外部キー有効化を保証する。統合テストで原子性と再試行・同時実行を検証する。
4. 起動処理で依存を組み立て、Bot・Webからユースケースを呼ぶ。ロール付与は購入のCOMMIT後に行う。

[DB設計](DATABASE.md)の3テーブルと既存の整合性条件を維持する。購入権は未返金の購入履歴から判定し、返金額は元の取引時の残高差から求める。出品者の残高不足で返金できない制約も引き継ぐ。

現在は設計書のみで移行対象の実装はないため、旧APIを残す委譲メソッドやデータ移行は不要。
