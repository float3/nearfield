# Nearfield

Every day, every new arXiv preprint, ranked by how far its authors are from
Munich — with a map, a timeline, the abstracts and introductions, and the
authors' email addresses, in an Android app.

## The problem this had to solve

No metadata service knows where today's authors are. Verified on 2026-10-27:

| Source | Has today's papers? | Affiliations? |
|---|---|---|
| arXiv RSS / OAI-PMH | yes, same day | **none** — 0 affiliation fields in 1,300 OAI records |
| Semantic Scholar | yes, same day | field present but **empty for every author** |
| OpenAlex *works* | **no** — 404 on today's ids, resolves at ~3 weeks old | yes, with coordinates |

So a same-day, distance-ranked digest cannot be built from paper metadata.

**What works instead is looking up the _author_ rather than the paper.** Most
authors have published before, and OpenAlex knows their last institution, with
a ROR id and coordinates. That places a brand-new paper with no PDF downloads
at all. It is a *prior*, not a fact — it was wrong about one author on day one,
putting them at University of Florida when their email said `unf.edu`, which is
University of North *Florida* — so the pipeline then reads the nearest papers'
own HTML to correct them, and every location carries the source it came from:

- `paper_html` — read off the paper itself. Authoritative.
- `author_prior` — inferred from publication history. Approximate; the author
  may have moved, and common names collide.
- `openalex_work` — OpenAlex has indexed the work.

The app shows that distinction on every pin and every row, so a ranking built
on a guess is never mistaken for one read off the paper.

## How a night runs

1. **Harvest** all 146 arXiv category feeds (`announce_type: new`, deduped by
   id). Not the archive-level feeds: `rss.arxiv.org/rss/cs` returns exactly
   2000 items, which is a cap that silently drops papers.
2. **Place** every author from publication history, cached on disk.
3. **Rank** by the nearest author on each paper.
4. **Read** the nearest ~50 papers' own HTML (`arxiv.org/html/<id>`) for
   affiliations, contact emails and the introduction — then re-rank, because
   corrections move papers.
5. **Publish** `data/<date>.json` plus the web app to GitHub Pages, and commit
   the warmed caches back so tomorrow is cheaper.

Introductions come from arXiv's LaTeXML HTML, not the PDF. A PDF's
cross-reference table lives at the *end* of the file, so a partial download
cannot be parsed at all, and a full one is ~10× the bytes for a worse result.

### Lookup budget

A cold cache needs roughly 12,000 OpenAlex author lookups for a full day, which
costs more than the free $1/day allowance. So `--max-lookups` caps each run and
the caches persist: papers left unplaced today get placed by later runs, and
after the first week most days need almost no network. Expect a sparse map on
day one — that is the cache warming, not a failure.

## Running it

```sh
cargo test --workspace
cargo run -p nearfield-cli -- run --date 2026-10-27 --prefetch 50
cargo run -p nearfield-cli -- fetch 2610.31702   # one paper, on demand
```

Environment (`.env`, gitignored):

```
OPENALEX_API_KEY=...      # free, from openalex.org/settings/api
NEARFIELD_CONTACT=you@example.com   # sent in the User-Agent, as arXiv asks
```

### The app

```sh
cd app && npm install && npm run dev     # browser, reads ../data directly
bash scripts/build-apk.sh                # signed, installable APK
```

`scripts/build-apk.sh` is the source of truth for the Android build:
`src-tauri/gen/` is generated and not committed, so the script regenerates it,
injects the signing config that the generated Gradle file lacks, and builds a
universal APK.

**The signing key in `signing/` is not in git — back it up.** An APK signed
with a different key cannot upgrade one signed with this one.

Requirements: Android SDK with NDK (r29 tested), and a JDK — Android Studio's
bundled `jbr` works.

### Releases and in-app updating

Two channels, both built by the `apk` workflow:

| Channel | Trigger | Tag | Where it lands |
|---|---|---|---|
| nightly | 03:00 UTC, if anything was committed in the last 24h | rolling `nightly` | prerelease, same download URL every time |
| stable | pushing a `v*` tag | that tag | normal release |

Each build writes `updates/<channel>.json`, which the app polls on launch and
compares against its own compiled-in version. A newer build raises a banner;
tapping it hands the APK to Android.

**Android will not let a sideloaded app update itself silently.** Tauri's
official updater is desktop-only — its README marks Android unsupported and its
dependencies are gated to `cfg(not(target_os = "android"))` — so this is a
hand-rolled check, and even a full native implementation would still hit the
system installer's consent dialog. One tap on the banner, one tap to confirm.

Allow "install unknown apps" for whichever app opens the link, once.

### Deployment

The `digest` workflow runs at 06:00 UTC, commits the day's JSON, and deploys to
Pages. Repository secrets:

| Secret | Why |
|---|---|
| `OPENALEX_API_KEY` | without it OpenAlex throttles the whole runner IP to a shared $0.10/day |
| `NEARFIELD_CONTACT` | courtesy contact address in the User-Agent; also compiled into the APK, so it is a secret rather than a literal in the source |
| `NTFY_TOPIC` | optional; a morning push via [ntfy](https://ntfy.sh) |
| `ANDROID_KEYSTORE_BASE64` | `base64 -w0 signing/nearfield.jks` — lets CI sign APKs |
| `ANDROID_KEYSTORE_PASSWORD` | the password from `signing/keystore.properties` |

Both workflows deploy Pages and share a `pages-deploy` concurrency group, so a
release and a digest cannot interleave and publish a half-built site.

## Being a good citizen

arXiv and OpenAlex both ask bulk users to identify themselves and throttle, so
every request carries a contact address and each host sits behind its own
minimum delay (arXiv 3s, OpenAlex 120ms). The per-paper HTML reads are capped
at the nearest ~50 a night rather than the full ~2,500, which is the difference
between ~10 MB and ~1 GB of someone else's bandwidth.

The emails are the ones authors print on their own papers for correspondence.
That is what they are for; the digest just saves you opening the PDF. It is
still a cold email — `mailto:` links are pre-addressed but never pre-sent.
