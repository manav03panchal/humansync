#!/bin/bash
set -e

cd "$(dirname "$0")/.."

TEAM_ID="J3UT3VLRKS"
DEVICE_ID="F46B9B1C-C1F1-587C-8A2A-94B9D0249DB3"
IPA_PATH="humandocs/src-tauri/gen/apple/build/arm64/Humandocs.ipa"
CONF="humandocs/src-tauri/tauri.conf.json"

# Set dev team for build
sed -i '' "s/\"developmentTeam\": \"\"/\"developmentTeam\": \"$TEAM_ID\"/" "$CONF"

# Build
cargo tauri ios build --debug

# Revert dev team
sed -i '' "s/\"developmentTeam\": \"$TEAM_ID\"/\"developmentTeam\": \"\"/" "$CONF"

# Install
xcrun devicectl device install app --device "$DEVICE_ID" "$IPA_PATH"

echo "Installed on iPhone."
