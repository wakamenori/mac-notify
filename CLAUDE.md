# CLAUDE.md

## プロジェクト概要

macOS の集中モード中に通知を収集し、タスク終了時に要約表示するメニューバー常駐アプリ。
緊急通知は Gemini 2.5 Flash Lite で判定し、集中モード中でも即時アラートを出す。

## 技術スタック

- Python 3.12+ / uv
- rumps（メニューバーアプリ）
- Gemini 2.5 Flash Lite（緊急度判定・要約）
- SQLite3（macOS 通知センター DB 読み取り）
- Ruff + Pyright（静的解析）
- pytest（テスト）

## ディレクトリ構成

```
src/           # アプリケーションコード
tests/         # テストコード
```

## コマンド

```bash
uv sync                      # 依存インストール
uv run ruff check .          # リント
uv run ruff format .         # フォーマット
uv run pyright               # 型チェック
uv run pytest                # テスト
uv run pytest --cov          # カバレッジ付きテスト
```

## ルール

- コードを変更・追加したら、必ず `uv run ruff check .` と `uv run pyright` を実行して問題がないことを確認する
- テストがあるコードを変更した場合は `uv run pytest` も実行する
