# ADR-0018: Exit codes classify the failure, and are part of the CLI's contract

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-63](https://linear.app/kzhq/issue/KEI-63)
* Extended by: [ADR-0009](0009-setup-diagnostics.md), which adds exit code 6 for
  `downer doctor`

## Context

`downer` exits 2, 3, 4, 5 or 6 for distinct classes of failure, 1 for the two
that have no class, and 0 on success. The constants are `src/lib.rs:24–31` and
the mapping is one `match` over every error variant, `exit_code()` at
`src/lib.rs:531`.

This predates the ADR directory. The codes were relied on before they were
recorded: `tests/cli.rs` asserts specific codes in a dozen places, ADR-0002
notes that conflicting cookie options exit 2, and ADR-0006 decided that
`FfmpegTooOld` exits 4 rather than 5 — a decision that only means something if
the numbers are a contract. Nothing said they were.

That gap is the risk worth closing. A code nobody has promised is a code a
contributor renumbers while tidying an enum, and the breakage is silent: a
script that treated 4 as "install FFmpeg" simply starts doing the wrong thing.

## Decision

**The exit codes are a public interface of the CLI, stable across versions, and
changing one is a breaking change.**

They classify by *what the user must do next*, which is the only thing a script
or a person can act on:

| Code | Class | What it means the caller should do |
| --- | --- | --- |
| 0 | success | — |
| 1 | unclassified | report it; this is a bug or an I/O failure on the host channel |
| 2 | invalid input | fix the arguments |
| 3 | output path | choose another path, or a conflict policy |
| 4 | FFmpeg unusable | install or upgrade FFmpeg |
| 5 | media or FFmpeg failure | the input or the network, not the installation |
| 6 | a setup check failed | `downer doctor` says which |

The distinction that does the work is **4 against 5**. Both look like "the
download did not happen", and the remedies have nothing in common: 4 is a
broken installation and no retry will help, 5 is a stream or a server and a
retry might. ADR-0006 put `FfmpegTooOld` on the 4 side for exactly that reason —
an FFmpeg too old to use is a broken installation, not a broken stream — and
ADR-0009 added 6 rather than reusing 4 because a setup failure is not always
FFmpeg's.

**One `match`, total, no fallback arm.** `exit_code()` enumerates every
`DownerError` variant explicitly. A new variant therefore fails to compile until
someone classifies it, which is the point: a `_ =>` arm would silently route
every future error to one code and the classification would rot without anyone
noticing.

**1 is for what cannot be classified**, and only `NativeIo` and `Host` land
there. Both belong to the native-messaging path, where the CLI's exit code is
not the channel anyone is reading — the extension sees the protocol's error
taxonomy instead (`docs/protocol.md`, "Error taxonomy"), and `src/main.rs` exits
1 directly for a host that fails to start. They are unclassified because for
them the number is not the interface.

### Why not one failure code

The rejected alternative is the ordinary Unix one: 0 or 1, details on stderr.
It fails the case this tool is actually in. The native host and `downer doctor`
both exist because the common failure here is *the installation*, not the
media, and telling those apart from a shell means parsing English error text
that no record promises to keep stable either.

### Why the numbers start at 2

1 is what a panic, a shell "command not found", and every convention-following
program already use for "something went wrong". Starting the classified codes at
2 keeps them distinguishable from the unclassified failures underneath.

## Consequences

* `tests/cli.rs` is the enforcement. It asserts codes 2, 3, 4 and 5 against real
  invocations, so a renumbering breaks the suite rather than a user's script.
* Adding an error variant is a compile error until it is classified. Adding a
  *code* is a change to this contract and needs an ADR amending this one — as
  ADR-0009 did for 6.
* The codes are not on the wire. The extension never sees them: the native host
  reports `error_code` strings from the protocol taxonomy, and the two
  vocabularies are independent by design — one names what the user must do, the
  other what the job did.
* `--help` and `README.md` both list the codes. They now have a record behind
  them rather than being a description of current behaviour.

## Unverified

* **No user of these codes is known.** The case for stability is made from the
  shape of the tool — a downloader called from scripts — rather than from an
  observed script that depends on one. Nothing is lost if none exists, and the
  cost of the promise is one `match` that already had to be written.
* **Codes above 6 are unallocated**, not reserved. Nothing checks that a future
  record does not reuse one, beyond this file being the place to look.
