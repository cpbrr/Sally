# Sally

Lightweight floating desktop app for live meeting transcription and
translation. Captures system audio and your microphone, shows the original
transcript above a live translation, and preserves the meeting as readable
Markdown. Design: [`2026-07-17-sally-live-translation-design.md`](./2026-07-17-sally-live-translation-design.md).

## Stack

- Tauri 2 shell, React + TypeScript UI (`src/`)
- Rust core services (`src-tauri/src/`): audio capture adapter, audio
  pipeline, Gemini Live client, timeline assembler, meeting store, cleanup
  service
- Gemini Live API for translation, Gemini Developer API for optional cleanup
  (model names configurable in `.env`)

## Development

```sh
npm install
npm run tauri dev     # run the app
npm run build         # typecheck + bundle the frontend
cd src-tauri && cargo test   # Rust unit tests
```

## Building executables

- **Windows portable exe:** `npx tauri build --no-bundle` →
  `src-tauri/target/release/sally.exe` (single file; needs only the
  WebView2 runtime that ships with Windows 11). Zip it with `.env.example`
  for the portable distribution.
- **macOS (Apple Silicon):** cannot be cross-compiled from Windows. The
  GitHub Actions workflow `.github/workflows/release.yml` builds both
  platforms — Windows ZIP + macOS `.app`/DMG — on every `v*` tag or manual
  dispatch. macOS signing/notarization (design §12.2) still needs Developer
  ID secrets.

## Multilingual meetings

One meeting can freely mix languages (e.g. English + Japanese + Vietnamese).
Gemini Live Translate detects the source language of each passage
automatically — no per-language configuration — and Sally sends
`translationConfig.targetLanguageCode` for the selected target.
`echoTargetLanguage` keeps transcript text even for speech already in the
target language. Known model limitation: detection can struggle with heavy
accents and very rapid mid-sentence switches.

## Translated-voice readout

A toggle (🔊 in the title bar, also in Settings, `SALLY_READOUT` in `.env`;
off by default) speaks the translated voice aloud. Playback is gated per
passage: speech already in the target language is never read out — with
target Vietnamese, English and Japanese speech is spoken in Vietnamese while
Vietnamese speech stays silent. Source language is classified locally from
the original transcript (exact for Japanese/Korean/Chinese/Thai/Vietnamese
scripts, coarse for plain-Latin languages). Use headphones: on speakers the
readout is captured back by loopback and re-enters the pipeline.

## Platform status

- **Windows 11 x64** — microphone capture and WASAPI loopback system audio
  implemented.
- **macOS 14+ (Apple Silicon)** — microphone works via cpal; the
  ScreenCaptureKit system-audio adapter is not implemented yet
  (`src-tauri/src/audio/capture.rs`), and signing/notarization is pending.

## Speakers

Timeline entries are attributed locally by audio activity: turns dominated
by microphone energy are labeled `You`, everything else `Meeting`. Labels
can be renamed or merged in the review screen after the meeting.

## Privacy

Meeting audio goes to Google's Gemini service; optional cleanup sends
transcript text. Free-tier API keys may allow Google to use content to
improve its products — unsuitable for confidential meetings unless approved.
When "Save meeting audio" is on (the default), a WAV recording is kept in
`meetings/audio/` on this device only and is never uploaded; turn it off in
Settings to store no audio at all. Sally collects no analytics and redacts
the API key from logs. The `.env` in the Sally data folder holds the key in
plain text by design.
