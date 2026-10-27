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

### Rate limits, and why a throttled run is fast

Keyless OpenAlex gives the **whole IP** a shared $0.10/day, and an institution
search costs $0.001 — about 100 lookups a day for everyone on that address,
which a CI runner exhausts instantly. Once spent, every request returns 429.

That is a *daily* budget, so it cannot clear inside a run. Retrying it is pure
waste: an early version spent 27 minutes doing nothing, because each lookup
burned four attempts at the server's `Retry-After` and the circuit breaker only
tripped after a dozen consecutive failures. Now a 429 gets at most one short
retry, a rate-limit error trips the breaker immediately, and `Retry-After` is
capped so no server can park the job for an hour. The same run takes 27 seconds
and still produces a digest from cache.

So a throttled night degrades to "today's papers, yesterday's locations" rather
than to a hung job — and `openalex_degraded` says so in the parsing report.

### What a day costs

OpenAlex prices by call, and the asymmetry drives the whole design
(figures from `X-RateLimit-Cost-USD`):

| Call | Cost | Used for |
|---|---|---|
| `authors?search=` | **$0.001** | placing an author from their history |
| `institutions?search=` | $0.001 | an affiliation read off a paper |
| `institutions/<id>`, `works/<id>` | **free** | following an id we already hold |

A real day holds **10,070 distinct author names**, so sweeping all of them costs
about **$10** against a free key's **$1/day**.

Exact matching would be ten times cheaper — `filter=display_name:` costs
$0.0001 — but it is useless here: arXiv writes *Nils Thuerey* where OpenAlex
stores *Nils Thürey*, and on thirty real names the exact filter matched **none**.
Fuzzy search is what makes this work, so the budget is the constraint.

Three things follow:

- **`--budget` is the real limit**, not a call count, since id lookups are free.
  The default leaves headroom under $1.
- **`--max-authors-per-paper` (3)** stops one paper eating the day. The largest
  paper in that sample had **251 authors**; uncapped it would spend a quarter of
  the allowance by itself. Cached authors are free and do not count.
- **The caches are committed**, so each run makes the next cheaper. Expect a
  sparse map for the first weeks — that is the cache warming, not a failure.

Affiliations are the better long-run investment: there are only so many
institutions, so that cache saturates, while new author names arrive every day.
Raising `--prefetch` buys authoritative locations at the cost of politeness time
rather than money.

## Measuring from somewhere else

Munich is the default, not an assumption. Every author and affiliation carries
its own coordinates, so changing the origin is arithmetic on data already on
the phone — instant, offline, no re-harvest.

In the app, the pill in the header opens **Measure from**:

- **Use my current location** — device GPS, so a trip re-ranks itself.
- **Look up a place** — any place name, or a bare `lat,lon`. Looked-up places
  are saved, so a destination is a one-tap switch afterwards.
- **Saved** — including the origin the digest was built against.

What the phone *cannot* do is read a paper it has no introduction for: the
nightly job prefetches the nearest ~50 from its own origin, so after moving the
origin the new top papers may need their **Fetch introduction & emails** button.
To avoid that before a trip, build a digest against the destination up front:

```sh
cargo run -p nearfield-cli -- run --origin "Kyoto" --prefetch 50
cargo run -p nearfield-cli -- where "Garching bei München"   # check a lookup
```

The `digest` workflow takes the same `origin` input, so a travel digest can be
dispatched from the Actions tab without a checkout.

## The parsing report

Extraction from arXiv's HTML is heuristic: LaTeXML's output varies by template,
and every publisher lays an author block out differently. So each run records
what it could not parse — and keeps the **raw strings**, not just counts, so a
fix can be written against real input instead of guessed at.

```sh
cargo run -p nearfield-cli -- report --samples
```

```
[FIX ]     9x Affiliation read off the paper matched no institution
         -> Look at the samples: if they share a shape, teach
            normalise_affiliation to strip it.
         . Equal contribution. University of Somewhere
         . *Corresponding author, Institute of Things
[note]   212x OpenAlex had no record of this author
```

Issues are split into **fix** (something in the parser could be better) and
**expected** (a paper with no HTML rendering, an author with no publication
history — nothing to do). Fixable issues sort first regardless of count, so a
single dropped paper outranks two hundred first-time authors.

The same report travels inside each digest and appears under the app's
**Parsing** tab, so the day's failures are visible on the phone without
digging. Both `affiliations_unresolved` and `intro_not_found` have already paid
for themselves — see the commit history for two parser fixes they produced.

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
