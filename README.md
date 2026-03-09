# mac-notify (Tauri)

macOS の集中モード中に通知を収集し、メニューバー常駐で扱う `Tauri + Rust + TypeScript` アプリ。

## できること

- メニューバー常駐
- 集中モード中の通知収集
- 集中モード終了時の要約表示
- 緊急通知の即時ダイアログ表示
- 手動要約（トレイメニュー）

## 技術構成

- `Tauri` (Rust backend)
- `TypeScript + Vite` (frontend shell)
- `rusqlite` (Notification DB 読み取り)
- `reqwest` + Ollama (既定: Qwen3.5、設定画面で切替可能なローカル LLM 緊急度判定)

## 必須条件

- macOS 15 (Tahoe) 以上
- フルディスクアクセス（Terminal / iTerm 等）
- [Ollama](https://ollama.com/) がインストール済みで `ollama serve` が起動していること
- `ollama pull qwen3.5:latest` などで利用したいモデルがダウンロード済みであること

## LLM モデル設定

- 初期値は `qwen3.5:latest`
- アプリの設定画面から、`ollama list` に表示されるローカルモデルを選択可能
- モデルのダウンロード自体はアプリでは行わず、事前に Ollama CLI で追加する

## 開発

```bash
npm install
npm run tauri:dev
```

## ビルド

```bash
npm run tauri:build
```

## 補足

- 旧 Python 実装から Tauri 実装へ移行済み。
- Ollama が起動していない場合、通知分析はフォールバック（中優先）で動作します。
