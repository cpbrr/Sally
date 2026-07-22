# Sally — project instructions for Claude

Tauri 2 + React/TS + Rust live-translation app. Windows and macOS (Apple
Silicon) both build automatically on every tagged release; Windows is the
primary dev/test target since this box can't run macOS itself. User
iterates via GitHub Releases.

## Build & test

- Rust: `cargo test --manifest-path src-tauri/Cargo.toml` (from repo root).
  No special env vars needed since v0.6.0 (sherpa-rs/bindgen removed).
- Frontend: `npx tsc --noEmit` for types, `npm run build` for the bundle.
- Run both before every commit. All tests must pass before shipping.

## Ship pipeline (do the whole thing unless told otherwise)

When a change is done and tested, run the full pipeline without asking:

1. Branch `feat/...` or `fix/...` off `main`.
2. Bump version in **three** files: `package.json`, `src-tauri/tauri.conf.json`,
   `src-tauri/Cargo.toml` (`Cargo.lock` updates on next build;
   `package-lock.json` is intentionally never bumped). Feature/rewrite =
   minor bump, fix = patch bump. Include the bump in the same PR branch.
3. Commit with `git commit -F <file>` — NEVER inline `-m` with here-strings
   (PowerShell quoting mangles them). Commit message: conventional title
   (`feat:`/`fix:`/`feat!:`), prose body wrapped ~70 cols, ending with:
   ```
   Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
   Claude-Session: <session url>
   ```
4. Push, `gh pr create`, then `gh pr merge <N> --merge` (merge commit, not
   squash).
5. On `main`: `git tag vX.Y.Z && git push origin vX.Y.Z`. CI (release.yml)
   builds the Windows portable ZIP and the macOS DMG in parallel and
   creates one draft release with both attached (~8-10 min; the release
   job waits on both platform jobs, mac is the slower leg). Watch with
   `gh run watch` in the background.
6. When CI is green, publish:
   `gh release edit vX.Y.Z --title "Sally vX.Y.Z" --notes-file <file> --draft=false --prerelease`
   ALWAYS `--prerelease`, NEVER `--latest` — the user promotes releases
   manually. **Every subsequent `gh release edit` on that tag (e.g. a
   later notes-only fix) must also re-pass `--prerelease`** — v0.17.1 and
   v0.18.1 both silently flipped to a full release after a follow-up
   `gh release edit ... --notes-file <file>` omitted it; caught and fixed
   after the fact. Check `gh release view vX.Y.Z --json isPrerelease`
   after any edit to that tag if in doubt.

## PR body format (reference: PR #15)

Prose paragraphs, no headers, no bullet lists of checkboxes, wrapped ~70
cols. Structure: what failed and the evidence → what the change does → side
effects/renames → test count. End with the same Co-Authored-By +
Claude-Session trailer as commits.

## Release notes format (reference: release v0.4.4)

- Title: `Sally vX.Y.Z` (exactly — past deviations were fixed manually).
- Body, in order:
  1. One-sentence summary line (plain language, user-facing benefit).
  2. `## Downloads` table — **always include the header + separator row**,
     even for a single-platform table (v0.14.0–v0.15.0 shipped without them:
     GitHub silently renders a headerless table as raw pipe-text, not a
     table — caught and fixed retroactively in v0.17.1). Both rows are the
     default now that mac builds automatically alongside Windows on every
     tag (see macOS build below) — the release job waits on both, so by
     the time the draft exists both files are already attached:
     ```
     | Platform | File | Notes |
     |---|---|---|
     | Windows 11 x64 | Sally-windows-x64-portable.zip | Portable — unzip anywhere, run Sally.exe. |
     | macOS 13+ Apple Silicon | Sally-macos-aarch64.dmg | Ad-hoc signed — right-click → Open on first launch, or `xattr -cr` if macOS calls it "damaged." |
     ```
     Only omit the macOS row for the rare release where the mac leg
     genuinely failed and shipping Windows alone couldn't wait — go back
     and add the row once a retroactive mac build lands (same manual-attach
     flow as macOS build below).
  3. `## Why <previous version> failed` (or `## Why remove it` etc.) —
     honest plain-language account of the problem.
  4. `## What changed` — bold-led bullets, user-facing wording.
  5. Closing "Expected behavior" line where it applies.

## macOS build (part of the default ship pipeline since v1.1.1)

Every `vX.Y.Z` tag push builds macOS alongside Windows (`release.yml`'s
`macos` job, `runs-on: macos-26` — required by the screencapturekit crate's
Swift bridge). The `release` job's `needs: [windows, macos]` means the
draft release only gets created once both finish, so a normal ship needs
no separate mac step — the DMG is already attached by the time you publish.

Manual dispatch still exists for one edge case: retroactively attaching a
mac build to an **older already-published release** (e.g. one shipped
before this policy changed, or where the mac leg failed and had to be
retried separately). Pin `--ref` to the exact tag being retrofitted, not
`main`, so the DMG matches that release's code — building against `main`
would attach a DMG running newer code than the release label says:
`gh workflow run release.yml --ref vX.Y.Z`, watch it, then
`gh run download <run-id> -n sally-macos-aarch64 -D <dir>` and
`gh release upload vX.Y.Z <dir>/Sally-macos-aarch64.dmg <dir>/Sally-macos-aarch64.dmg.sha256`
onto that release. Remember to add the macOS row to that release's notes
per above — it's easy to attach the asset and forget the table.

## After shipping

- Update the graphify knowledge graph: `/graphify . --update` (graph lives
  in `graphify-out/`; it is untracked, leave it that way).
- Update auto-memory (`sally-project-state.md`) with what shipped and any
  open leads.

## Project conventions & gotchas

- Speaker labels are "You" (mic-dominated) or "Meeting" (everything else),
  full stop. LOCAL diarization is banned in any form: live variants failed
  six tuning releases (removed v0.6.0, history in git v0.4.0–v0.5.1), and
  the offline WAV pass shipped in v0.8.0 made no practical difference and
  was removed in v0.9.0. Speaker attribution is the Gemini cleanup step's
  job (`cleanup_prompt` instructs it to infer and label speakers in the
  polished file; the raw file keeps You/Meeting).
- Meeting transcripts append to the raw file as entries seal; nothing runs
  at meeting end (keep it that way — the end-meeting hang was a regression
  we fixed).
- The two-stage assembler (`rotate_turn`) routes lagging translation into
  the closing entry so original/translated stay paired. Any change that
  splits or seals entries must preserve this.
- `.env` in the user's data folder is plain-text by design; never log or
  echo `GEMINI_API_KEY` (redaction in `config.rs`).
- `src-tauri/target/` grows unbounded (tens of GB after heavy iteration).
  If disk is a concern, `cargo clean --manifest-path src-tauri/Cargo.toml`
  is always safe.
- Design doc: `2026-07-17-sally-live-translation-design.md` (historical §7
  diarization sections are obsolete).
- Readout has no language gate (removed v0.17.0): every remote (Meeting)
  passage plays translated audio regardless of source language, source ==
  target included. `echoTargetLanguage: true` in `gemini/live.rs` must stay
  true — it's what makes Gemini produce audio for same-language passages
  at all. Mic (You) speech is never read back translated, by design, not a
  bug — it only ever reaches the transcript (`open_mic_dominated()` check
  in `session.rs`'s Audio handler). The only other playback gate is
  `repeat_loop_muted` (v0.17.1): Gemini's live-translate model has a known
  server-side failure mode where it gets stuck verbatim-repeating a phrase
  (seen on both the original transcript and the translation, most often
  but not only when the target is Vietnamese) — `lang::has_repeat_loop()`
  detects that specific signature and mutes for the rest of the turn. It
  is a mitigation for one known failure shape, not a general fix; if
  garbled/looping audio resurfaces without visible text repetition, this
  won't catch it.
