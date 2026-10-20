# 実装の入口

このリポジトリの実装順序・工程別の対象・完了条件・Codexへの依頼文は [design/IMPLEMENTATION.md](design/IMPLEMENTATION.md) にある。

実装を依頼されたら、指定工程と参照設計を読み、現在のコード・進捗表を照合する。既存実装を再利用し、必要な型とメソッドから追加する。全体実装の依頼では工程順に進める。

仕様の参照先は [design/CONTENT.md](design/CONTENT.md)、[design/DATABASE.md](design/DATABASE.md)、[design/REPOSITORY.md](design/REPOSITORY.md)、[design/ARCHITECTURE.md](design/ARCHITECTURE.md)。

実装後は該当工程を検証し、進捗表を更新する。未実施の実機確認は未確認として残し、実行した検証と次の作業を報告する。

テスト名は条件と期待する振る舞いを日本語で表す。機能追加・不具合修正では仕様のテストリストから一つ選び、失敗を確認してから最小限の実装で成功させる。既存コードのリファクタリングは、振る舞いを確認するテストを成功させた状態で行う。
