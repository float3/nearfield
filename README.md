# Nearfield

A daily list of new arXiv preprints, sorted by how far each paper's nearest author is from Munich or another place you choose, shown in an Android app.

## Usage

```sh
cargo run -p nearfield-cli -- run                       # build today's digest into data/
cargo run -p nearfield-cli -- run --origin "Kyoto"      # measure from somewhere else
cargo run -p nearfield-cli -- fetch 2610.31702          # fetch one paper's introduction and emails
cargo run -p nearfield-cli -- report --samples          # what the parser failed on today
```

Put these in `.env`:

```
OPENALEX_API_KEY=...                 # free key from openalex.org/settings/api
NEARFIELD_CONTACT=you@example.com    # sent in the User-Agent
```

Without a key, OpenAlex allows the whole IP $0.10 a day, and author locations come only from the cache.

## App

```sh
cd app && npm install && npm run dev    # in the browser, reading ../data
bash scripts/build-apk.sh               # signed APK; needs the Android SDK, NDK and a JDK
```

The signing key in `signing/` is not in git. Back it up: an APK signed with another key cannot update an installed one.

## CI

- `digest` runs daily at 06:00 UTC, commits `data/<date>.json` and the caches, and deploys GitHub Pages.
- `apk` builds the nightly channel on every code commit to `main`, and the stable channel on a `v*` tag. The app checks `updates/<channel>.json`, downloads a newer build and opens Android's installer.

Secrets: `OPENALEX_API_KEY`, `NEARFIELD_CONTACT`, `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, and optionally `NTFY_TOPIC` for a morning notification.
