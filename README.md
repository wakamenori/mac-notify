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
- `reqwest` (Gemini API 呼び出し)

## 必須条件

- macOS 15 (Tahoe) 以上
- フルディスクアクセス（Terminal / iTerm 等）
- `.env` に `GOOGLE_API_KEY` 設定

`.env` 例:

```bash
GOOGLE_API_KEY=your_google_api_key
```

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
- `.env` は起動時に自動読込されます（`dotenvy`）。
