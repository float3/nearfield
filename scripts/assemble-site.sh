#!/usr/bin/env bash
# Build the Pages site: the web app, the published digests, and the release
# channel manifests the Android app polls for updates.
#
# Shared by both workflows so the two cannot drift and clobber each other's
# deploy with a partial site.
set -euo pipefail
cd "$(dirname "$0")/.."

rm -rf site
mkdir -p site/data site/updates

( cd app && npm ci --silent && npm run build )
cp -r app/dist/. site/

# Digests. Missing data is not fatal — a fresh repo has none yet.
if compgen -G "data/*.json" >/dev/null; then
  cp data/*.json site/data/
else
  echo "[]" > site/data/index.json
fi

# Update manifests, written by the apk workflow.
if compgen -G "updates/*.json" >/dev/null; then
  cp updates/*.json site/updates/
fi

# Pages serves no directory listings, and Jekyll would eat files starting
# with an underscore.
touch site/.nojekyll
echo "site assembled:"
find site -maxdepth 2 -type f | head -20
