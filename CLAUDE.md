# Sally — project instructions for Claude

Tauri 2 + React/TS + Rust live-translation app. Windows is the only active
target (macOS CI job is manual-dispatch only). User iterates via GitHub
Releases.

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
   builds the Windows portable ZIP and creates a draft release (~8-10 min).
   Watch with `gh run watch` in the background.
6. When CI is green, publish:
   `gh release edit vX.Y.Z --title "Sally vX.Y.Z" --notes-file <file> --draft=false --prerelease`
   ALWAYS `--prerelease`, NEVER `--latest` — the user promotes releases
   manually.

## PR body format (reference: PR #15)

Prose paragraphs, no headers, no bullet lists of checkboxes, wrapped ~70
cols. Structure: what failed and the evidence → what the change does → side
effects/renames → test count. End with the same Co-Authored-By +
Claude-Session trailer as commits.

## Release notes format (reference: release v0.4.4)

- Title: `Sally vX.Y.Z` (exactly — past deviations were fixed manually).
- Body, in order:
  1. One-sentence summary line (plain language, user-facing benefit).
  2. `## Downloads` table:
     `| Windows 11 x64 | Sally-windows-x64-portable.zip | Portable — unzip anywhere, run Sally.exe. |`
  3. `## Why <previous version> failed` (or `## Why remove it` etc.) —
     honest plain-language account of the problem.
  4. `## What changed` — bold-led bullets, user-facing wording.
  5. Closing "Expected behavior" line where it applies.

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
