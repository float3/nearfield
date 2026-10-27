#!/usr/bin/env python3
"""Inject a release signing config into Tauri's generated Gradle build.

`app/src-tauri/gen/` is generated and not committed, so `tauri android init`
rewrites it on every build and the signing config has to be re-applied each
time. Idempotent: a second run is a no-op.

Usage: inject-signing.py <path-to-app/build.gradle.kts>
"""

import sys
from pathlib import Path

MARKER = "// nearfield-signing"

LOADER = """
// nearfield-signing
val keystorePropertiesFile = rootProject.file("keystore.properties")
val keystoreProperties = Properties().apply {
    if (keystorePropertiesFile.exists()) {
        keystorePropertiesFile.inputStream().use { load(it) }
    }
}
"""

SIGNING_CONFIG = """    signingConfigs {
        create("release") {
            if (keystorePropertiesFile.exists()) {
                storeFile = file(keystoreProperties["storeFile"] as String)
                storePassword = keystoreProperties["storePassword"] as String
                keyAlias = keystoreProperties["keyAlias"] as String
                keyPassword = keystoreProperties["keyPassword"] as String
            }
        }
    }
"""


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    if not path.exists():
        print(f"not found: {path} — run `tauri android init` first", file=sys.stderr)
        return 1

    src = path.read_text(encoding="utf8")
    if MARKER in src:
        print("signing config already present")
        return 0

    if "android {" not in src:
        print("unexpected build.gradle.kts: no android block", file=sys.stderr)
        return 1

    src = src.replace("android {", LOADER + "\nandroid {", 1)
    src = src.replace("android {\n", "android {\n" + SIGNING_CONFIG, 1)
    src = src.replace(
        '        getByName("release") {\n',
        '        getByName("release") {\n'
        '            signingConfig = signingConfigs.getByName("release")\n',
        1,
    )
    path.write_text(src, encoding="utf8")
    print("signing config injected")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
