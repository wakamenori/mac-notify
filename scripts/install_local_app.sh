#!/bin/sh
set -eu

APP_NAME="Notify"
BUNDLE_ID="com.wakamenori.notify"
SOURCE_APP="src-tauri/target/release/bundle/macos/${APP_NAME}.app"
TARGET_APP="/Applications/${APP_NAME}.app"

if [ ! -d "$SOURCE_APP" ]; then
  echo "Built app not found: $SOURCE_APP" >&2
  exit 1
fi

osascript -e "quit app \"${APP_NAME}\"" >/dev/null 2>&1 || true
sleep 2
pkill -x notify >/dev/null 2>&1 || true

i=0
while pgrep -x notify >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -ge 5 ]; then
    pkill -9 -x notify >/dev/null 2>&1 || true
    sleep 1
    break
  fi
  sleep 1
done

if pgrep -x notify >/dev/null 2>&1; then
  echo "Failed to stop ${APP_NAME}.app" >&2
  pgrep -fl notify >&2 || true
  exit 1
fi

rm -rf "$TARGET_APP"
cp -R "$SOURCE_APP" "$TARGET_APP"

codesign --force --deep --sign - --identifier "$BUNDLE_ID" "$TARGET_APP"

open "$TARGET_APP"
echo "Installed and launched $TARGET_APP"
