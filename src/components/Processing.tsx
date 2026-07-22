// Post-meeting flow. SavedPopup: small confirmation right after End
// Meeting. ProcessingScreen: full-window pre-processing menu — pick any
// past meeting from the raw folder, rename it, choose timestamps,
// optionally AI-clean (which also attributes speakers), then open the
// result.

import { convertFileSrc } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState } from "react";
import {
  api,
  formatTimestamp,
  MeetingFile,
  ReviewInfo,
  TranscriptChunk,
} from "../api";
import { useSally } from "../store";
import { useShallow } from "zustand/react/shallow";
import { IconFolder, IconSwap } from "./Icons";

export function SavedPopup() {
  const dict = useSally((s) => s.dict);
  const setPhase = useSally((s) => s.setPhase);
  return (
    <div className="overlay">
      <div className="sheet">
        <h2>{dict.savedTitle}</h2>
        <p>{dict.savedBody}</p>
        <div className="row end">
          <button className="btn" onClick={() => setPhase("idle")}>
            {dict.close}
          </button>
          <button className="btn primary" onClick={() => setPhase("processing")}>
            {dict.goToProcessing}
          </button>
        </div>
      </div>
    </div>
  );
}

function RecordingPlayer({
  review,
  audioPath,
  chunkLang,
  onToggleChunkLang,
}: {
  review: ReviewInfo;
  audioPath: string;
  chunkLang: "original" | "translated";
  onToggleChunkLang: () => void;
}) {
  const dict = useSally((s) => s.dict);
  const [chunks, setChunks] = useState<TranscriptChunk[]>([]);
  const audioRef = useRef<HTMLAudioElement>(null);

  // Clickable transcript blocks for the recording, refreshed whenever a
  // meeting with a recording is (re)opened.
  useEffect(() => {
    if (review?.audio_path) {
      api.meetingChunks().then(setChunks).catch(() => setChunks([]));
    } else {
      setChunks([]);
    }
  }, [review?.raw_path, review?.audio_path]);

  const jumpTo = (ms: number) => {
    const el = audioRef.current;
    if (!el) return;
    el.currentTime = ms / 1000;
    el.play().catch(() => {});
  };

  return (
    <div className="recording">
      <div className="row">
        <h3 style={{ flex: 1 }}>{dict.recordingTitle}</h3>
        {chunks.length > 0 && (
          <button
            className={`btn compact ${
              chunkLang === "translated" ? "primary" : "secondary"
            }`}
            title={dict.toggleChunkLang}
            onClick={onToggleChunkLang}
          >
            <IconSwap />
          </button>
        )}
      </div>
      <audio
        ref={audioRef}
        controls
        preload="metadata"
        src={convertFileSrc(audioPath)}
        style={{ width: "100%" }}
      />
      {chunks.length > 0 && (
        <>
          <p className="field-hint">{dict.recordingHint}</p>
          <div className="chunk-list">
            {chunks.map((c, i) => (
              <button
                key={i}
                className="chunk-btn"
                onClick={() => jumpTo(c.start_ms)}
              >
                <span className="meta">{formatTimestamp(c.start_ms)}</span>
                <span className="speaker">{c.speaker}</span>
                <span className="chunk-text">
                  {chunkLang === "translated"
                    ? c.translated || c.text
                    : c.text || c.translated}
                </span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

export function ProcessingScreen() {
  const { dict, review, setReview, setPhase } = useSally(
    useShallow((s) => ({
      dict: s.dict,
      review: s.review,
      setReview: s.setReview,
      setPhase: s.setPhase,
    }))
  );
  const [meetings, setMeetings] = useState<MeetingFile[]>([]);
  const [meetingTitle, setMeetingTitle] = useState("");
  const [includeTimestamps, setIncludeTimestamps] = useState(true);
  const [includeOriginal, setIncludeOriginal] = useState(false);
  const [aiCleanup, setAiCleanup] = useState(true);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [resultPath, setResultPath] = useState<string | null>(null);
  // Owned here (not inside RecordingPlayer) so the toggle survives that
  // component unmounting/remounting when switching away from a meeting
  // with a recording and back.
  const [chunkLang, setChunkLang] = useState<"original" | "translated">(
    "translated"
  );

  // Load the meeting list; open the newest when nothing is selected yet.
  useEffect(() => {
    api
      .listMeetings()
      .then((list) => {
        setMeetings(list);
        if (!review && list.length > 0) {
          selectMeeting(list[0].path);
        }
      })
      .catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const selectMeeting = async (path: string) => {
    setError("");
    setResultPath(null);
    setMeetingTitle("");
    try {
      const info = await api.openMeeting(path);
      setReview(info);
    } catch (e) {
      setError(String(e));
    }
  };

  const run = async () => {
    if (!review) return;
    setError("");
    setRunning(true);
    try {
      const updated = await api.applyReview(meetingTitle.trim() || undefined);
      setReview(updated);
      // One timestamps choice for everything: when excluded, the export
      // copy is written and the AI cleanup also omits them.
      let result = updated.raw_path;
      if (!includeTimestamps) {
        result = await api.exportWithoutTimestamps();
      }
      if (aiCleanup) {
        result = await api.cleanAndSummarize(includeTimestamps, includeOriginal);
      }
      setResultPath(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="processing">
      <div className="processing-inner">
        <div className="row">
          <h2 style={{ flex: 1 }}>{dict.processingTitle}</h2>
          {review && (
            <button
              className="btn compact"
              title={dict.openRawFolder}
              onClick={() => openPath(review.raw_dir)}
            >
              <IconFolder />
            </button>
          )}
        </div>

        <label>
          {dict.meetingPick}
          <select
            value={review?.raw_path ?? ""}
            onChange={(e) => selectMeeting(e.target.value)}
          >
            {!review && <option value="">—</option>}
            {review && !meetings.some((m) => m.path === review.raw_path) && (
              <option value={review.raw_path}>{review.raw_path}</option>
            )}
            {meetings.map((m) => (
              <option key={m.path} value={m.path}>
                {m.name}
              </option>
            ))}
          </select>
        </label>

        {review?.audio_path && (
          <RecordingPlayer
            review={review}
            audioPath={review.audio_path}
            chunkLang={chunkLang}
            onToggleChunkLang={() =>
              setChunkLang((l) => (l === "original" ? "translated" : "original"))
            }
          />
        )}

        {review && resultPath === null && (
          <>
            <label>
              {dict.meetingName}
              <input
                type="text"
                value={meetingTitle}
                onChange={(e) => setMeetingTitle(e.target.value)}
                placeholder="Untitled meeting"
              />
            </label>

            <label className="check">
              <input
                type="checkbox"
                checked={includeTimestamps}
                onChange={(e) => setIncludeTimestamps(e.target.checked)}
              />
              {dict.includeTimestamps}
            </label>
            <p className="field-hint">{dict.includeTimestampsHint}</p>

            <label className="check">
              <input
                type="checkbox"
                checked={includeOriginal}
                onChange={(e) => setIncludeOriginal(e.target.checked)}
              />
              {dict.includeOriginal}
            </label>
            <p className="field-hint">{dict.includeOriginalHint}</p>

            <label className="check">
              <input
                type="checkbox"
                checked={aiCleanup}
                onChange={(e) => setAiCleanup(e.target.checked)}
              />
              {dict.aiCleanup}
            </label>
            <p className="field-hint">{dict.aiCleanupHint}</p>

            {error && <p className="error-text">{error}</p>}

            <div className="row end">
              <button className="btn" onClick={() => setPhase("idle")}>
                {dict.backToApp}
              </button>
              <button className="btn primary" onClick={run} disabled={running}>
                {running ? dict.processingRunning : dict.runProcessing}
              </button>
            </div>
          </>
        )}

        {review && resultPath !== null && (
          <>
            <p className="ok-text">{dict.processingSuccess}</p>
            <p className="field-hint">{resultPath}</p>
            <div className="row end">
              <button className="btn" onClick={() => setResultPath(null)}>
                {dict.backToApp}
              </button>
              <button className="btn" onClick={() => setPhase("idle")}>
                {dict.backToHome}
              </button>
              <button
                className="btn primary"
                onClick={() => openPath(resultPath)}
              >
                {dict.openMarkdown}
              </button>
            </div>
          </>
        )}

        {!review && error && <p className="error-text">{error}</p>}
        {!review && (
          <div className="row end">
            <button className="btn" onClick={() => setPhase("idle")}>
              {dict.backToApp}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
