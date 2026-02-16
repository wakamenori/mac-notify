# mac-notify

macOS の集中モード中に通知を裏で収集し、メニューバーからいつでも要約を確認できる常駐アプリ。
緊急度の高い通知は集中モード中でも即時アラートを出す。

## 概要

```
[通知センター SQLite DB] → ポーリング → [緊急度判定]
                                          ├─ 緊急 → display dialog で即時表示
                                          └─ 通常 → 蓄積 → メニューバーから要約確認
[rumps メニューバーアプリ]
  ├─ 通知要約の一覧表示
  ├─ 通知元アプリへジャンプ
  └─ 集中モード ON/OFF 状態表示
```

## 機能

### P0（MVP）
- 集中モード中の通知を SQLite DB からポーリングで収集
- メニューバーから収集した通知の要約を確認

### P1
- 緊急通知の即時アラート（`display dialog` で集中モード貫通）
- 通知元アプリへのジャンプ（`open -a` / URL scheme）

### P2
- LLM 判定プロンプトのカスタマイズ
- 通知履歴の保存・検索

## 技術スタック

- Python 3.12+
- [uv](https://github.com/astral-sh/uv) — パッケージ管理
- [rumps](https://github.com/jaredks/rumps) — メニューバー常駐アプリ
- Gemini 2.5 Flash Lite — 緊急度判定・要約生成
- SQLite3 — macOS 通知センター DB の読み取り
- osascript — 緊急アラート表示（`display dialog`）
- plistlib — 通知データ（binary plist）のパース

## 通知取得の仕組み

macOS の通知センターは SQLite DB に通知を保存している。

```
# macOS 15+ (Sequoia/Tahoe)
~/Library/Group Containers/group.com.apple.usernoted/db2/db

# macOS 13-14 (Ventura/Sonoma)
$(getconf DARWIN_USER_DIR)/com.apple.notificationcenter/db2/db
```

このDBをポーリングして新しい通知を検出する。
macOS 15+ では TCC 同意ダイアログが表示されるため、初回起動時にユーザーの許可が必要。

## 集中モード検知

```
~/Library/DoNotDisturb/DB/Assertions.json
```

を読み取ることで、現在の集中モードの状態を判定する。

## 緊急アラートの仕組み

`display notification` は集中モードに従い抑制されるが、
`display dialog` はモーダルウィンドウとして通知パイプラインを経由しないため、集中モード中でも表示される。

これを利用して、緊急と判定した通知のみ `display dialog` で即時表示する。

## 必要な権限

- **フルディスクアクセス**（macOS 15+）: 通知センター DB の読み取りに必要
- ターミナル / Python に対して許可を付与する

## 開発

```bash
uv sync
```

