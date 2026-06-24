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
- Codex CLI (reasoning effort: low で通知の緊急度判定)

## 必須条件

- macOS 15 (Tahoe) 以上
- フルディスクアクセス（Terminal / iTerm 等）
- Codex CLI がインストール済みで、必要に応じて `codex login` 済みであること

## LLM 設定

- 通知分析は `codex exec` で実行する
- reasoning effort は `low` 固定
- 設定画面では Codex CLI の利用状態を確認できる

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
- Codex CLI を利用できない場合、通知分析はフォールバック（中優先）で動作します。
