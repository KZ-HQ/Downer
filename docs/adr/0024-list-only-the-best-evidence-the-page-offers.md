# ADR-0024: The popup lists only the best evidence the page offers

* Status: Accepted
* Date: 2026-09-22
* Issue: [KEI-98](https://linear.app/kzhq/issue/KEI-98)
* Follows: [ADR-0011](0011-one-playlist-parser.md), which is why detection is one
  rule in two languages and why this record deliberately stays out of it

## Context

On the ordinary HTML5 compatibility pattern —

```html
<video controls>
  <source src="mov_bbb.mp4" type="video/mp4">
  <source src="mov_bbb.ogg" type="video/ogg">
</video>
```

— the popup listed two downloads. Nothing was malfunctioning: both URLs are
media, both are on the page, and `.ogg` belongs in the detection list. But
sibling `<source>` elements are **alternatives, not contents**. They are one
video in two containers, present so a browser can pick whichever it can play.
Firefox played the `.mp4` and never fetched the `.ogg`. Offering both made a
page with one video look like a page with two, on the first screen a new user
sees.

## What the obvious fix would have cost

The issue proposed collapsing `<source>` siblings, and required that the
collapse "key off sharing a parent `<video>`, not off filenames looking
similar". **Neither scanner can ask that question.**

`src/scraper.rs::extract_media_urls` has no DOM at all — it is a regex over raw
HTML text. Giving it parent-child knowledge means an HTML-parsing dependency the
crate does not have, or a hand-rolled `<video …>…</video>` block matcher, which
is regex-as-parser and breaks on nested markup and on `>` inside an attribute
value.

`extension/content.js` has the DOM and discards the structure one line before
the scan runs: `querySelectorAll` becomes a flat `attributeValues: string[]`.
Cheap to change, but it is an interface change to the one function the two
languages must mirror.

So the cost of a sibling rule is not the rule. It is giving the CLI structural
knowledge of a page it has never had, in order to answer a question the
extension can already answer another way.

## Decision

**The popup lists only the best evidence tier present, and folds the rest into
"Other candidates". Detection does not change, and the CLI does not change.**

`media-scan.js` already grades every candidate: `observed` (the page fetched
it), `declared` (it is in the markup), `inferred` (it matched in the page text).
On the reported page the `.mp4` is `observed` and the `.ogg` is `declared`, so
the evidence already says which one matters. No rule about sibling elements is
needed, and none is used.

The rule is `extension/candidate-view.js::partition`, named and placed to
parallel `job-view.js`: that module decides which *jobs* the popup renders, this
one decides which *candidates* it lists. Both are rendering decisions that read
a vocabulary defined elsewhere rather than defining one.

This is a generalisation of what the popup already did, not a new mechanism. The
old split was the same idea with `observed` and `declared` merged into one tier.

### Two limits, both deliberate

**A playlist is never demoted.** `collectCandidates` sorts playlists ahead of
files unconditionally, and ranking a `declared` `.m3u8` below an `observed`
preview clip would contradict a rule the code already holds. The stream is what
the user came for even when the player has not started fetching it.

**Nothing is dropped.** A demoted candidate renders under the disclosure
triangle and is counted in its summary. The summary now distinguishes its two
reasons — "found in the page text" for an `inferred` match, "this page did not
load" for a `declared` one — because the old wording would have been false for
the new case.

### The CLI keeps listing everything, and that is not a divergence

`--list` shows every candidate, as before. Listing is what a listing command is
for, and the CLI has no `observed` signal to rank with: nothing is fetched, so
every candidate would be in the same tier and the rule would be inert there
anyway.

This does not break the invariant in `AGENTS.md` that the CLI and the extension
find the same media. They still do — `media-scan.js` and `scraper.rs` are
untouched and `tests/fixtures/media-extensions.json` still pins them. What
differs is presentation, in one surface, using a signal only that surface has.

### It degrades to the old behaviour rather than to a wrong one

Open the popup before playback and nothing is `observed`, so `declared` is the
best tier present and the split is exactly what it was. The failure mode is "no
change", not a wrong answer. That is the main reason this was preferred to
collapsing to the first `<source>`, whose failure mode is offering the file the
browser did not play.

## What was rejected

**Collapse to the observed sibling, falling back to the first.** Inert in the
CLI — nothing is ever `observed` without a browser — so the CLI would sit
permanently in the fallback branch. It buys a rarely-fired refinement at the
price of the two sides disagreeing in exactly the case the refinement exists
for.

**Collapse to the first `<source>`.** Deterministic and spec-aligned, but a page
listing `.ogg` first would get us offering the file Firefox did not play. That
is worse than listing both, because at least listing both contains the right
answer. Convention puts the preferred format first; nothing here measures how
often convention holds.

**One row with a format picker**, the way renditions are a quality picker. The
tidiest concept and by far the most expensive: a new candidate shape on the
wire, `--list` output changes, and `--json` is a versioned public interface
under [ADR-0020](0020-json-output-is-a-cli-interface.md), so its schema moves.
It also only half-solves the problem — the popup would stop claiming two videos
but would still ask a question the user has no basis to answer. Nobody wants the
`.ogg`.

**Leaving it.** Defensible, and free. Rejected because the cost lands entirely on
a new user's first screen.

## Consequences

* A page whose player has loaded its media offers exactly that media, and the
  rest is one click away under "Other candidates".
* The fix generalises past `<source>`: a preview clip, an ad, or a stale URL in
  the markup is demoted for the same reason, on any page where something was
  actually fetched.
* `popup.html` now loads `media-scan.js`, because `candidate-view.js` reads
  `CONFIDENCE` from it rather than restating the order. The popup's script list
  and the jsdom harness's must stay in step; the harness fails with an undefined
  global if they drift, which is the intended way to find out.
* A page with two genuinely distinct videos where the user has played only one
  now lists one and demotes the other. It is still visible and still counted.
  This is the rule's real cost.
* The headline count describes what is listed, not what was found, as it already
  did for `inferred` candidates. The collapsed summary carries the difference.

## Unverified

* **That `observed` is reliable in the popup's timing window.** It reads
  `performance.getEntriesByType("resource")` at scan time. A page that fetches
  its media long after load, or one whose entries have been cleared by
  `performance.clearResourceTimings()`, presents as all-`declared` and gets the
  old behaviour. That is the safe direction, but it is not measured.
* **How often a page has two real videos and only one played.** The rule's cost
  case is judged rare from the pages this project has looked at. No survey backs
  that.
* **The rendered result in Firefox.** The split is unit-tested and jsdom-tested.
  Getting a headless Firefox to actually play a `<video>` so that a `<source>`
  becomes `observed` was judged too fragile to pin in `tests/e2e/`, so the
  browser-level check is manual.
