#!/bin/bash
# macOS でテスト通知を送信するスクリプト
# Usage: ./scripts/send_test_notifications.sh [数] or ./scripts/send_test_notifications.sh single "タイトル" "本文"

send_notification() {
  local title="$1"
  local body="$2"
  local subtitle="${3:-}"

  if [ -n "$subtitle" ]; then
    osascript -e "display notification \"$body\" with title \"$title\" subtitle \"$subtitle\""
  else
    osascript -e "display notification \"$body\" with title \"$title\""
  fi
}

# 単発送信モード
if [ "$1" = "single" ]; then
  title="${2:-テスト通知}"
  body="${3:-これはテスト通知です}"
  send_notification "$title" "$body"
  echo "送信: $title - $body"
  exit 0
fi

# バッチ送信モード（デフォルト: 5件）
count="${1:-5}"

notifications=(
  "Slack|山田太郎|明日の会議の資料を共有しました"
  "Slack|佐藤花子|緊急: 本番環境でエラーが発生しています"
  "メール|田中一郎|【重要】契約更新のお知らせ"
  "LINE|鈴木次郎|今日のランチどうする？"
  "カレンダー|リマインダー|15:00 チームミーティング開始"
  "GitHub|PR Review|feat: add notification filtering が承認されました"
  "Slack|プロジェクトch|デプロイ完了しました 🚀"
  "メール|人事部|月末の勤怠提出をお忘れなく"
  "LINE|母|今週末帰ってくる？"
  "Slack|bot|CI/CD パイプラインが失敗しました"
)

total=${#notifications[@]}
if [ "$count" -gt "$total" ]; then
  count=$total
fi

echo "テスト通知を ${count} 件送信します..."

for ((i = 0; i < count; i++)); do
  IFS='|' read -r app sender body <<< "${notifications[$i]}"
  send_notification "$app" "$body" "$sender"
  echo "  [$((i + 1))/${count}] $app: $sender - $body"
  sleep 1
done

echo "完了"
