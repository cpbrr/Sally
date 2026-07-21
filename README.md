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
off by default) speaks the translated voice aloud for every remote (Meeting)
passage — no per-language gating, so source == target (e.g. Vietnamese
dubbed into Vietnamese) reads out same as any other pair. Your own mic
speech is never read back translated; it only ever reaches the raw
transcript. Capture is scoped to one selected app/tab (per-app loopback on
Windows, the Core Audio tap on macOS), so Sally's own readout is
structurally excluded from what it captures — no echo/loopback concern, and
no headphones requirement.

## Platform status

- **Windows 11 x64** — microphone capture and WASAPI loopback system audio
  implemented.
- **macOS 13+ (Apple Silicon)** — microphone via cpal; system audio via
  ScreenCaptureKit (`src-tauri/src/audio/sck_capture.rs`) — no BlackHole
  needed. Grant Screen Recording permission when prompted; a BlackHole-style
  loopback device still works as fallback if permission is denied. The DMG
  is unsigned (signing/notarization pending): right-click → Open on first
  launch.

## Speakers

Entries are attributed locally by audio activity: turns dominated by
microphone energy are labeled `You`, everything else `Meeting`. Remote
lines still split per voice (segmentation model on the system lane), but
they all keep the `Meeting` label in the raw transcript. The optional AI
clean & summarize step attributes speakers: Gemini works out who is
speaking from the conversation itself and labels remote lines per person
in the polished file, keeping the raw file untouched.

## Privacy

Meeting audio goes to Google's Gemini service; optional cleanup sends
transcript text. Free-tier API keys may allow Google to use content to
improve its products — unsuitable for confidential meetings unless approved.
When "Save meeting audio" is on (the default), a WAV recording is kept in
`meetings/audio/` on this device only and is never uploaded; turn it off in
Settings to store no audio at all. Sally collects no analytics and redacts
the API key from logs. The `.env` in the Sally data folder holds the key in
plain text by design.
