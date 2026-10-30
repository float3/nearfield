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
# Android's versionCode must rise with every build a phone should accept as
# an update. Each release gets a block of 10000 codes:
#
#   stable X                 base(X) * 10000 + 9999
#   nightly after release X  base(X + 1 patch) * 10000 + commits since tag vX
#
# base(a.b.c) = a*10000 + b*100 + c. So stable 0.1.2 < every nightly built on
# 0.1.2 < stable 0.1.3: moving from stable to nightly, and from a nightly to
# the next stable, are both upgrades. Going from a nightly back to the stable
# it was built on is a downgrade, which Android refuses whatever the numbers.
# A rebuild of the same commit gets the same code, which Android accepts.
version_code() {
  local channel="$1" base="$2" major minor patch n=0
  IFS=. read -r major minor patch <<<"${base%%[-+]*}"
  if ((minor > 99 || patch > 99)); then
    echo "version $base does not fit the versionCode scheme" >&2
    return 1
  fi
  local block=$((major * 10000 + minor * 100 + patch))
  if [[ "$channel" == "stable" ]]; then
    echo $((block * 10000 + 9999))
    return
  fi
  # Before the release tag is pushed, the release commit itself counts as 0.
  if git rev-parse -q --verify "refs/tags/v$base" >/dev/null; then
    n=$(git rev-list --count "v$base..HEAD")
  fi
  ((n > 9998)) && n=9998
  echo $(((block + 1) * 10000 + n))
}
VERSION_CODE="${NEARFIELD_VERSION_CODE:-$(version_code "$CHANNEL" "$BASE_VERSION")}"
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

# 4. Build, stamping the version. `tauri android build` rewrites
#    gen/android/app/tauri.properties from the config on every run, so writing
#    that file beforehand did nothing: every nightly shipped as 0.1.2 with
#    versionCode 1002. A config override is what the build reads.
#    Universal APK so it runs on any phone.
npx tauri android build --apk --config \
  "{\"version\":\"$VERSION\",\"bundle\":{\"android\":{\"versionCode\":$VERSION_CODE}}}"

OUT="$ROOT/app/src-tauri/gen/android/app/build/outputs/apk/universal/release"
echo
echo "APK(s) in $OUT:"
ls -la "$OUT" | grep -i apk || true
