# Sally

Lightweight floating desktop app for live meeting transcription and
translation. Captures system audio and your microphone, shows the original
transcript above a live translation, and preserves the meeting as readable
Markdown. Design: [`2026-07-17-sally-live-translation-design.md`](./2026-07-17-sally-live-translation-design.md).

## Stack

- Tauri 2 shell, React + TypeScript UI (`src/`)
- Rust core services (`src-tauri/src/`): audio capture adapter, audio
  pipeline, diarization, Gemini Live client, timeline assembler, meeting
  store, cleanup service
- Gemini Live API for translation, Gemini Developer API for optional cleanup
  (model names configurable in `.env`)

## Development

```sh
npm install
npm run tauri dev     # run the app
npm run build         # typecheck + bundle the frontend
cd src-tauri && cargo test   # Rust unit tests
```

## Platform status

- **Windows 11 x64** — microphone capture and WASAPI loopback system audio
  implemented.
- **macOS 14+ (Apple Silicon)** — microphone works via cpal; the
  ScreenCaptureKit system-audio adapter is not implemented yet
  (`src-tauri/src/audio/capture.rs`), and signing/notarization is pending.

## Diarization models

Speaker labels are best-effort and local (design §7). The production ONNX
VAD/speaker-embedding models are still to be selected; they plug in through
the `EmbeddingExtractor` trait in `src-tauri/src/diarization.rs`. Until then
a built-in spectral-band profile provides coarse separation, and diarization
can be disabled in Settings.

## Privacy

Meeting audio goes to Google's Gemini service; optional cleanup sends
transcript text. Free-tier API keys may allow Google to use content to
improve its products — unsuitable for confidential meetings unless approved.
Sally stores no audio, no analytics, and redacts the API key from logs. The
`.env` in the Sally data folder holds the key in plain text by design.
