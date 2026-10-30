#!/usr/bin/env bash
# Is there an arXiv list out that has no digest yet?
#
# Prints `date=<YYYY-MM-DD>` and `new=true|false`, in the form GitHub's
# $GITHUB_OUTPUT takes. Cheap: a few feed fetches, no build. The digest
# workflow runs this before the expensive steps, so it can try every hour
# without harvesting the same list twice.
#
# A digest is named after the evening arXiv sent its list out, which is the
# day before the date the feeds stamp on it (see arxiv::announced_on). Several
# feeds are read because one can be empty or stale on its own: during arXiv's
# migration only cs.AI had items, and they were the previous day's.
set -euo pipefail

cd "$(dirname "$0")/.."
FEEDS=(cs.LG hep-th math.AP astro-ph.GA)
UA="nearfield (+https://github.com/float3/nearfield)"

latest=""
for f in "${FEEDS[@]}"; do
  body=$(curl -fsS --max-time 30 -A "$UA" "https://rss.arxiv.org/rss/$f" || true)
  stamp=$(grep -m1 -oE '<pubDate>[^<]+' <<<"$body" | sed 's/<pubDate>//' || true)
  [[ -z "$stamp" ]] && continue
  # "Fri, 30 Oct 2026 00:00:00 -0400": the calendar date as arXiv wrote it.
  day=$(date -u -d "$(awk '{print $2, $3, $4}' <<<"$stamp")" +%F)
  [[ "$day" > "$latest" ]] && latest="$day"
done

if [[ -z "$latest" ]]; then
  echo "no feed carried a dated item" >&2
  echo "new=false"
  exit 0
fi

sent=$(date -u -d "$latest -1 day" +%F)
echo "date=$sent"
if [[ -f "data/$sent.json" ]]; then
  echo "new=false"
else
  echo "new=true"
fi
