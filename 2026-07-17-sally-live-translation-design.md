# Sally Live Translation and Transcription Design

**Date:** 2026-07-17  
**Status:** Approved design  
**Initial platforms:** Windows 11 x64 and Apple Silicon macOS 14+

## 1. Product Summary

Sally is a lightweight floating desktop application for employees who attend meetings in languages they do not speak fluently. It captures system audio and the employee's microphone, displays the original transcript and a live translation in two stacked panels, and preserves the meeting as readable Markdown.

The first release supports Windows 11 x64 and Apple Silicon MacBooks running macOS 14 or newer. Both releases are developed from one shared Tauri application so their behavior and interface remain consistent. Platform-specific native adapters handle audio capture and operating-system permissions.

Sally is deliberately narrow in scope. It does not include chat, interview coaching, meeting bots, dashboards, attachments, cloud accounts, audio playback, or audio recordings.

## 2. Goals and Success Criteria

Sally must:

- Capture system audio and microphone input during meetings.
- Translate Japanese, English, and other automatically detected source languages into a user-selected Gemini-supported target language.
- Display the original transcript above the live translation.
- Preserve timestamps and best-effort speaker labels during the meeting.
- Support meetings lasting up to four hours.
- Save a continuously updated raw Markdown transcript locally.
- Export a timestamp-free copy without altering the preserved raw transcript.
- Optionally generate a cleaned transcript and structured meeting summary.
- Run as a resizable floating window that can remain above meeting applications and move across monitors.
- Provide English and Vietnamese interface languages selected during initial setup.
- Avoid retaining audio on disk.

The first release is successful when the complete workflow passes on both supported platforms: setup, permission grant, meeting capture, live transcription, live translation, speaker review, raw export, optional cleanup, crash recovery, and reopening the generated files.

## 3. Non-Goals

The first release will not provide:

- Cloud synchronization or user accounts.
- A company administration dashboard.
- Meeting scheduling or calendar integration.
- A meeting bot that joins calls as a participant.
- Audio recording or translated-audio playback.
- Guaranteed speaker identity or biometric identification.
- Support for Windows 10, Intel Macs, or macOS versions older than 14.
- Mobile or browser versions.

## 4. Technology and Architecture

### 4.1 Application stack

- **Desktop shell:** Tauri.
- **Interface:** React and TypeScript.
- **Core services:** Rust.
- **Local inference:** ONNX Runtime.
- **Live translation:** Gemini Live API using `gemini-3.5-live-translate-preview` by default.
- **Cleanup:** Gemini Developer API using `gemini-3.1-flash-lite` by default.

Both Gemini model identifiers are configurable in `.env` because preview availability, model names, and free-tier access can change.

### 4.2 Module boundaries

The application is divided into small services with explicit interfaces:

1. **Audio Capture Adapter**
   - Produces timestamped microphone and system-audio frames.
   - Uses WASAPI loopback and microphone capture on Windows.
   - Uses ScreenCaptureKit and Core Audio on macOS.
   - Contains no Gemini or UI logic.

2. **Audio Pipeline**
   - Resamples, normalizes, and mixes audio for Gemini's mono PCM input.
   - Retains separate source frames long enough to label microphone speech as `You` and diarize remote speech.
   - Uses bounded in-memory buffers and never writes audio to disk.

3. **Diarization Service**
   - Runs voice-activity detection and speaker embeddings locally.
   - Clusters remote speech into temporary speaker identities.
   - Returns timestamp ranges and confidence-bearing labels.
   - Can be disabled or replaced without changing transcript storage or UI code.

4. **Gemini Live Client**
   - Maintains the Live API WebSocket connection.
   - Sends audio in the format and cadence required by the selected model.
   - Receives original input transcription and translated output transcription.
   - Discards translated audio output without playing or saving it.
   - Reconnects with bounded retry behavior and reports explicit gaps.

5. **Timeline Assembler**
   - Aligns original transcript fragments, translated fragments, timestamps, audio sequence numbers, and diarization ranges.
   - Emits stable timeline entries to the interface and persistence layer.
   - Allows recent provisional labels to stabilize without repeatedly changing older visible passages.

6. **Meeting Store**
   - Appends finalized timeline entries to raw Markdown.
   - Maintains a temporary recovery journal for incomplete entries and speaker assignments.
   - Performs safe finalization, export, renaming, and recovery.

7. **Cleanup Service**
   - Reads a finalized transcript only after the user requests cleanup.
   - Processes long transcripts in bounded sections.
   - Requests structured output, consolidates it, and renders polished Markdown.
   - Never overwrites the raw transcript.

8. **Application UI**
   - Renders state received from the Rust core.
   - Does not capture audio, call Gemini directly, or write meeting files directly.

## 5. Live Meeting Data Flow

1. The user selects a target language and starts a meeting.
2. Sally captures microphone and system audio as separate streams.
3. Microphone speech is marked as `You`. System audio is passed through local diarization.
4. The audio pipeline resamples and mixes the streams into the mono PCM stream required by Gemini Live Translate.
5. Gemini returns original-language input transcription and target-language output transcription. Sally silently discards translated audio bytes.
6. The timeline assembler aligns both text streams with the local session clock, audio sequence numbers, speech boundaries, and speaker ranges.
7. The interface updates both panels while the meeting store appends finalized entries to Markdown.
8. Temporary audio frames are discarded immediately after the live API and diarization pipelines no longer need them.

The session clock is monotonic so system clock changes do not corrupt timestamps. Wall-clock time is used only for meeting metadata and filenames.

## 6. Interface Design

### 6.1 Visual direction

The interface takes inspiration from the supplied dark floating-panel reference while removing every unrelated feature. It should feel calm, compact, and professional rather than like a full meeting suite.

The window contains:

- A slim title bar with the Sally name, connection status, pin control, target-language selector, and Settings.
- An upper `Transcript` panel containing timestamps, speaker labels, and original speech.
- A lower `Live Translation` panel containing corresponding translated passages.
- A compact session bar containing elapsed time, Start or Pause, and End Meeting.
- A draggable divider between the two panels.

The panels remain vertically stacked at every supported window size. The window is resizable, remembers its size, position, monitor, and pin state, and enables always-on-top by default. A visible pin control disables this behavior.

### 6.2 Scrolling behavior

Both panels follow the current passage during normal use. When the user scrolls backward, Sally pauses auto-scroll without pausing capture. New entries continue to arrive, and a `Jump to live` control returns both panels to the latest passage.

### 6.3 First-run setup

First launch guides the user through:

1. Choosing English or Vietnamese for Sally's interface.
2. Entering a Gemini API key.
3. Selecting a writable Sally data folder.
4. Reviewing the free-tier privacy disclosure.
5. Granting microphone and system-audio permissions.
6. Running a short audio and API connectivity test.

Settings remain editable later, including interface language, data folder, audio devices, model identifiers, diarization toggle, and always-on-top default.

### 6.4 End-of-meeting review

Ending a meeting enters a review state that provides:

- A compact list for globally renaming or merging generated speaker labels.
- An `Include timestamps` export option.
- Open raw Markdown.
- Export raw Markdown copy.
- `Clean & Summarize`.
- Open polished Markdown after successful processing.

The raw transcript is already preserved before any cleanup action begins.

## 7. Speaker Diarization

Gemini Live Translate does not supply remote-speaker labels. Sally therefore performs best-effort diarization locally.

The diarization pipeline uses:

- A small voice-activity detection model to find speech segments.
- A speaker-embedding model to convert each remote speech segment into a numerical voice representation.
- Online clustering to group similar representations into `Speaker 1`, `Speaker 2`, and later labels.

Microphone input is always labeled `You`. Remote speech with insufficient confidence is labeled `Meeting`. Overlapping speech is labeled `Multiple speakers` unless one voice is clearly dominant.

The model recognizes voice similarity, not identity. Users rename speakers after the meeting. They can also merge two labels when the diarizer split one person into multiple clusters.

Only embeddings and transcript metadata survive after their source audio frames are discarded. A final reconciliation pass may improve clustering from retained embeddings but cannot replay or recover discarded audio.

The exact VAD and speaker-embedding models will be selected during implementation. A candidate must have commercial redistribution rights, acceptable CPU and memory use on both target architectures, reasonable accuracy for Japanese, English, and Vietnamese speech, and an offline bundle size appropriate for a desktop utility.

## 8. Local Files and Configuration

The user selects a Sally data folder during setup. This avoids writing beside an installed macOS application and provides consistent behavior across platforms.

```text
Sally Data/
|-- .env
|-- meetings/
|   |-- 2026-07-17_1430-untitled-raw.md
|   `-- 2026-07-17_1430-untitled-polished.md
`-- .recovery/
```

The `.env` file intentionally contains the API key as plain text, as requested. It also contains configurable model identifiers and advanced settings. Setup and documentation warn that anyone who can read or copy this folder can obtain the key. Sally redacts the key from every log and error message.

Meeting filenames begin with local start date and time. The user may rename a meeting after it ends, which renames all associated files together.

### 8.1 Raw Markdown

The raw file contains meeting metadata followed by timestamped source and translation entries:

```markdown
[00:18] **Speaker 1**

Original: next Friday deadline question

Vietnamese: Vietnamese translation of the passage
```

The actual file contains the verbatim model transcripts rather than the illustrative text above. Connection gaps are represented explicitly. The preserved raw file always retains timestamps. A timestamp-free export creates a separate copy and does not remove information from the source.

### 8.2 Recovery journal

Sally appends finalized passages throughout the meeting. A small recovery journal contains only incomplete textual timeline state and speaker assignments. It never contains audio. After an interruption, the next launch offers to recover the meeting into Markdown. Successful finalization removes the journal.

## 9. Cleanup and Summarization

Cleanup is manual and optional. Sally sends the finalized transcript to `gemini-3.1-flash-lite` or the replacement configured in `.env`.

The polished file contains:

1. Concise meeting summary.
2. Key decisions.
3. Action items with owners and deadlines when explicitly stated.
4. Open questions.
5. Cleaned transcript.

The cleanup prompt requires the model to preserve meaning, remove filler and false starts, avoid inventing facts, mark uncertainty, respect final speaker names, and include timestamps only when selected.

Long transcripts are split into bounded sections. Each section is cleaned independently, after which a final structured consolidation request produces the meeting-level summary. Partial processing remains temporary. Sally publishes the polished Markdown file only after all required stages succeed.

Cancellation, quota exhaustion, rate limits, unavailable models, or network failures never modify the raw transcript. The UI reports the failure and permits retry.

## 10. Privacy and Security

Sally stores no audio recordings and includes no analytics or telemetry. Diagnostic logs exclude audio, transcript content, and API keys.

Live meeting audio is nevertheless sent to Google's Gemini service, and optional cleanup sends transcript text to Google. During setup, Sally states this plainly and requires acknowledgment.

Google's current Gemini Developer API pricing documentation states that free-tier content may be used to improve Google products, while paid-tier content is not. Sally therefore warns that free-tier keys may be unsuitable for confidential company meetings unless the company has approved that use. Sally does not attempt to infer whether a key belongs to a free or paid project and does not claim enterprise privacy guarantees.

The plain-text `.env` design is an explicit usability tradeoff. It is not presented as secure secret storage.

## 11. Error Handling

- Invalid or missing API key: block session start and link to setup.
- Missing permission: explain the required permission before opening the relevant OS settings.
- Audio device unavailable: pause capture, preserve the current transcript, and offer another device.
- Gemini disconnect: retry with bounded exponential backoff while preserving the local timeline.
- Unrecoverable transcription interval: insert a visible gap marker with its time range.
- Translation-only failure: continue original transcription when possible and mark translation unavailable.
- Rate or quota limit: keep the session and raw transcript intact and display an actionable message.
- App close during meeting: require confirmation.
- Crash, sleep, or forced shutdown: recover finalized text and journaled state on next launch.
- Data-folder write failure: warn immediately, pause new capture, and allow selecting a writable folder.

No failure mode silently discards known transcript content or creates a polished file that appears complete when processing was partial.

## 12. Platform Packaging

### 12.1 Windows

- Target Windows 11 x64.
- Distribute a portable ZIP containing Sally and required native libraries.
- Store user data in the folder selected during setup rather than assuming the executable directory is writable.

### 12.2 macOS

- Target Apple Silicon and macOS 14 or newer.
- Sign with a Developer ID, enable the hardened runtime, notarize the release, and staple the ticket.
- Explain and request Microphone and Screen Recording permissions. ScreenCaptureKit requires screen-capture authorization to access application or system audio.
- Keep editable `.env` and transcript files outside the signed application bundle in the selected Sally data folder.

## 13. Testing Strategy

### 13.1 Automated tests

- Audio conversion, mixing, bounded buffering, and sequence numbering.
- Transcript and translation fragment assembly.
- Timestamp generation and speaker-range alignment.
- Diarization clustering behavior using licensed synthetic or consented fixtures.
- Low-confidence and overlapping-speaker fallbacks.
- Markdown serialization, timestamp removal, renaming, and recovery.
- Cleanup chunking, structured-response validation, and final rendering.
- English and Vietnamese localization completeness.
- Window-state persistence logic.
- Error-state transitions and retry limits.

A fake Gemini WebSocket server simulates partial responses, out-of-order timing, disconnects, rate limits, and reconnects without consuming API quota.

### 13.2 Manual platform matrix

The release checklist covers Zoom, Microsoft Teams, browser meetings, and local media on both supported platforms. It verifies:

- System audio and microphone capture together.
- Japanese and English speech translated into Vietnamese.
- Additional selected target languages.
- Audio-device changes during an active session.
- Multi-monitor movement, resizing, pinning, and restored placement.
- Permission denial and recovery.
- Speaker renaming and merging.
- Raw and polished output files.
- Timestamped and timestamp-free exports.
- No audio files or sensitive diagnostic logs.

### 13.3 Longevity and recovery

- Simulate four-hour meetings and confirm bounded memory use.
- Force Gemini reconnects throughout a long session.
- Terminate Sally during writes and verify recovery.
- Sleep and wake each platform during an active session.
- Fill or revoke access to the data folder and verify safe handling.

## 14. Release Artifacts

Each release provides:

- Windows 11 x64 portable ZIP.
- Signed and notarized Apple Silicon macOS distribution.
- SHA-256 checksums.
- Sample `.env` without credentials.
- English and Vietnamese setup documentation.
- A short privacy and company-approval notice.

Windows and macOS builds are released together after the shared acceptance workflow passes on both platforms.

