#!/usr/bin/env bash
# Build a signed, installable Nearfield APK.
#
# gen/android is generated and not committed, so this script is the source of
# truth: it regenerates the Android project, injects the signing config that
# the generated build.gradle.kts does not carry, and builds.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

export JAVA_HOME="${JAVA_HOME:-/c/Program Files/Android/Android Studio/jbr}"
export ANDROID_HOME="${ANDROID_HOME:-$LOCALAPPDATA/Android/Sdk}"
if [[ -z "${NDK_HOME:-}" ]]; then
  NDK_HOME="$(find "$ANDROID_HOME/ndk" -maxdepth 1 -mindepth 1 -type d | sort -V | tail -1)"
  export NDK_HOME
fi

# Release channel. CI sets these; a local build is an untagged stable build.
CHANNEL="${NEARFIELD_CHANNEL:-stable}"
BASE_VERSION="$(python -c "import json;print(json.load(open('app/src-tauri/tauri.conf.json'))['version'])")"
if [[ "$CHANNEL" == "nightly" && -z "${NEARFIELD_VERSION:-}" ]]; then
  NEARFIELD_VERSION="${BASE_VERSION}+nightly.$(date -u +%Y%m%d).$(git rev-parse --short HEAD 2>/dev/null || echo local)"
fi
VERSION="${NEARFIELD_VERSION:-$BASE_VERSION}"
VERSION_CODE="${NEARFIELD_VERSION_CODE:-$(date -u +%y%m%d)}"
# Compiled into the binary, so the app knows what it is and what to poll.
export NEARFIELD_CHANNEL="$CHANNEL"
export NEARFIELD_VERSION="$VERSION"
# Contact address for the arXiv User-Agent. Kept out of the source, so take it
# from the environment or the gitignored .env.
if [[ -z "${NEARFIELD_CONTACT:-}" && -f "$ROOT/.env" ]]; then
  NEARFIELD_CONTACT="$(grep -E '^NEARFIELD_CONTACT=' "$ROOT/.env" | cut -d= -f2- || true)"
fi
export NEARFIELD_CONTACT="${NEARFIELD_CONTACT:-}"
if [[ -z "$NEARFIELD_CONTACT" ]]; then
  echo "warning: NEARFIELD_CONTACT unset; the app will identify itself with a placeholder"
fi
echo "Building $VERSION ($CHANNEL, versionCode $VERSION_CODE)"

KEYSTORE="$ROOT/signing/nearfield.jks"
KEYPROPS="$ROOT/signing/keystore.properties"

# 1. One-time keystore. Kept out of git; back it up, because an APK signed with
#    a different key cannot upgrade one signed with this one.
if [[ ! -f "$KEYSTORE" ]]; then
  PW="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 24)"
  "$JAVA_HOME/bin/keytool" -genkeypair -v \
    -keystore "$KEYSTORE" -alias nearfield \
    -keyalg RSA -keysize 4096 -validity 10000 \
    -storepass "$PW" -keypass "$PW" \
    -dname "CN=Nearfield, O=Nearfield, C=DE" >/dev/null
  KS_NATIVE="$(cygpath -m "$KEYSTORE" 2>/dev/null || echo "$KEYSTORE")"
  cat > "$KEYPROPS" <<EOF
storeFile=$KS_NATIVE
storePassword=$PW
keyAlias=nearfield
keyPassword=$PW
EOF
  echo "Created a new signing key at $KEYSTORE"
  echo "Its password is in $KEYPROPS — back both up."
fi

# 2. Regenerate the Android project.
cd "$ROOT/app"
# A clean checkout has no node_modules, so `npm run build` would not find tsc.
if [[ ! -d node_modules ]]; then
  if [[ -f package-lock.json ]]; then npm ci; else npm install; fi
fi
npm run build
npx tauri android init >/dev/null

# 3. Inject signing into the generated gradle build.
GEN="$ROOT/app/src-tauri/gen/android"
cp "$KEYPROPS" "$GEN/keystore.properties"
python "$ROOT/scripts/inject-signing.py" "$GEN/app/build.gradle.kts"
python "$ROOT/scripts/inject-manifest.py" "$GEN/app/src/main/AndroidManifest.xml"

# 4. Stamp the version. Android requires a monotonically increasing
#    versionCode, so nightlies use the date; tauri regenerates this file on
#    every init, hence writing it here.
cat > "$GEN/app/tauri.properties" <<EOF
tauri.android.versionCode=$VERSION_CODE
tauri.android.versionName=$VERSION
EOF

# 5. Build. Universal APK so it runs on any phone.
npx tauri android build --apk

OUT="$ROOT/app/src-tauri/gen/android/app/build/outputs/apk/universal/release"
echo
echo "APK(s) in $OUT:"
ls -la "$OUT" | grep -i apk || true
