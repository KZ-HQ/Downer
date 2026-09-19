# ADR-0005: Default to `video.<ext>`, and make title naming opt-in

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-84](https://linear.app/kzhq/issue/KEI-84)
* Supersedes: the **naming** half of
  [ADR-0004](0004-output-naming-and-collision-policy.md). Its collision half —
  rename for an inferred name, fail for an exact path, reserve exclusively,
  release an unused reservation — stands unchanged, with the suffix respelled.

## Context

ADR-0004 named a generically named download after the page title, falling back
to the source host. The reasoning was sound and the implementation works: it is
covered by unit, integration and real-Firefox end-to-end tests.

What it was missing was evidence for its premise. Naming a file after
`document.title` is only better than a fixed default if real page titles are
worth reading, and nobody had checked. KEI-82 was created to check, and could
not: the judgement needs a human on an unrestricted network looking at real
sites, and an agent sandbox has neither. The mechanical half of that issue was
finished; the judgement sat open.

Two things then settled it. The question was **not blocking anything** — nobody
was failing to download a file because of the name. And a title is a worse
default than it first looks: it varies by site, by login state, by locale, and
by whatever marketing put in the `<title>`, so the filename a user gets is
unpredictable in a way a fixed name never is. The cost of finding out whether
titles are good was higher than the value of the answer.

## Decision

### A generically named download is `video.<ext>`

When the URL-derived stem is generic — `index`, `playlist`, `master`,
`download`, `video`, `media`, or digits only — or there is no stem at all, the
name is `video` plus the extension the media-type logic already chose. That is
the whole default. A distinctive URL filename still wins, so direct-file
downloads are untouched; only the `index.m3u8` case reaches this.

**The source-host fallback is removed.** It existed to distinguish two sites
whose playlists were both `index.m3u8`, and it only ever fired when no title was
supplied — which is now the common case rather than the exception. `localhost.mp4`
or `cdn-eu-3.example.com.mp4` is not what "default" should mean, and the
collision policy already prevents the two sites from destroying each other's
files.

### Title naming stays, behind an explicit opt-in

The machinery is not deleted, because it works and because the judgement that
deferred it may yet go the other way. It is reached two ways:

* **CLI:** `--name`, which was always an explicit opt-in and is unchanged.
* **Extension:** a Settings checkbox, *off* by default. When it is off,
  `background.js` does not send `title` at all.

That last detail is why this needs no protocol change. Opting in is expressed by
**sending the field or not**, so no `name_from_title` flag joins the wire,
`protocol_version` stays at 1, and a host that predates the setting sees exactly
what it saw before. `title` remains a documented optional field; it is simply
not sent by default.

### The collision suffix is `_2`, not ` (2)`

A respelling, not a policy change. `_2` needs no quoting in a shell, survives
tools that treat spaces or parentheses as separators, and is the form asked for.
Everything else about collisions — who renames, who fails, the exclusive
reservation, releasing an unused one — is ADR-0004's and unchanged.

## Consequences

* **Uniqueness is kept; meaningfulness is given up.** A folder of HLS downloads
  becomes `video.mp4`, `video_2.mp4`, `video_3.mp4`, indistinguishable without
  opening them. ADR-0004's objective was "meaningful and unique"; this keeps the
  half that prevents data loss and drops the half that required an unanswered
  judgement. That is the accepted trade, not an oversight — a user who wants the
  other half ticks one box.
* Renaming matters *more* than before, not less. Under ADR-0004 a collision
  needed two downloads of the same page; now every generically named download
  wants the same name, so the rename sequence is what stops them overwriting
  each other. It is correspondingly better tested.
* Nothing that already worked regresses: a URL with a real filename is named
  from it, exactly as before both ADRs.
* KEI-82's remaining step is moot. Whether real page titles read well no longer
  decides anything by default, and the opt-in is the user's call on their own
  pages.

## Unverified

* Whether real page titles make good filenames is **still unknown**, and is now
  a question about the opt-in rather than about the default. The end-to-end test
  exercises the setting against a fixture whose title we chose, which proves the
  plumbing and says nothing about the web.
* No user has been observed choosing between the two. The guess that a
  predictable default beats a derived one is a judgement about what people
  expect from a downloader, not a measured preference.
