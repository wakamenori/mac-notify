# CLAUDE.md

## プロジェクト概要

macOS の集中モード中に通知を収集し、タスク終了時に要約表示するメニューバー常駐トレイアプリ。
緊急通知は Gemini で判定し、集中モード中でも即時アラートを出す。

## 技術スタック

- Rust（バックエンド） + TypeScript（フロントエンド）
- Tauri v2（トレイアプリフレームワーク）
- Vite（フロントエンドビルド）
- Gemini API（緊急度判定）
- rusqlite（macOS 通知センター DB 読み取り）

## ディレクトリ構成

```
src/                # フロントエンド (TypeScript)
src-tauri/          # Rust バックエンド
  src/
    main.rs         # エントリポイント
    commands.rs     # Tauri コマンド
    db.rs           # SQLite 操作
    focus.rs        # 集中モード検知
    gemini.rs       # Gemini API 連携
    models.rs       # データモデル
    orchestrator.rs # オーケストレーション
scripts/            # ユーティリティスクリプト
```

## コマンド

```bash
npm install                          # フロントエンド依存インストール
npm run tauri:dev                    # 開発モード起動
npm run tauri:build                  # プロダクションビルド
cd src-tauri && cargo clippy         # Rust リント
cd src-tauri && cargo fmt            # Rust フォーマット
cd src-tauri && cargo check          # Rust 型チェック
```

## ルール

- Rust コードを変更・追加したら、必ず `cd src-tauri && cargo clippy` と `cargo fmt --check` を実行して問題がないことを確認する
- TypeScript コードを変更したら、`npm run build` でビルドエラーがないことを確認する
