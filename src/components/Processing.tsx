// Post-meeting flow. SavedPopup: small confirmation right after End
// Meeting. ProcessingScreen: full-window pre-processing menu — pick any
// past meeting from the raw folder, rename it and its speakers, choose
// timestamps, optionally AI-clean, then open the result.

import { convertFileSrc } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState } from "react";
import { api, formatTimestamp, MeetingFile, TranscriptChunk } from "../api";
import { useSally } from "../store";
import { IconFolder } from "./Icons";

export function SavedPopup() {
  const { dict, setPhase, diarizeState } = useSally();
  return (
    <div className="overlay">
      <div className="sheet">
        <h2>{dict.savedTitle}</h2>
        <p>{dict.savedBody}</p>
        {diarizeState === "running" && (
          <p className="field-hint">{dict.diarizeRunning}</p>
        )}
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

export function ProcessingScreen() {
  const { dict, review, setReview, setPhase, diarizeState } = useSally();
  const [meetings, setMeetings] = useState<MeetingFile[]>([]);
  const [meetingTitle, setMeetingTitle] = useState("");
  const [names, setNames] = useState<Record<string, string>>({});
  const [includeTimestamps, setIncludeTimestamps] = useState(true);
  const [aiCleanup, setAiCleanup] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [resultPath, setResultPath] = useState<string | null>(null);
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

  // Background speaker identification finished: reload the meeting so the
  // new "Speaker N" labels appear — but never over the user's typed
  // renames or a finished processing run.
  useEffect(() => {
    if (diarizeState !== "done" || !review || resultPath !== null) return;
    const untouched = Object.entries(names).every(([o, n]) => n === o);
    if (untouched) {
      selectMeeting(review.raw_path);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [diarizeState]);

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
    if (review) {
      setNames(Object.fromEntries(review.speakers.map((s) => [s, s])));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const selectMeeting = async (path: string) => {
    setError("");
    setResultPath(null);
    setMeetingTitle("");
    try {
      const info = await api.openMeeting(path);
      setReview(info);
      setNames(Object.fromEntries(info.speakers.map((s) => [s, s])));
    } catch (e) {
      setError(String(e));
    }
  };

  const renameable = (review?.speakers ?? []).filter(
    (s) => s !== "You" && s !== "Meeting"
  );

  const run = async () => {
    if (!review) return;
    setError("");
    setRunning(true);
    try {
      const renames = Object.fromEntries(
        Object.entries(names).filter(([o, n]) => n.trim() && n.trim() !== o)
      );
      const updated = await api.applyReview(
        renames,
        meetingTitle.trim() || undefined
      );
      setReview(updated);
      // One timestamps choice for everything: when excluded, the export
      // copy is written and the AI cleanup also omits them.
      let result = updated.raw_path;
      if (!includeTimestamps) {
        result = await api.exportWithoutTimestamps();
      }
      if (aiCleanup) {
        result = await api.cleanAndSummarize(includeTimestamps);
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
          <div className="recording">
            <h3>{dict.recordingTitle}</h3>
            <audio
              ref={audioRef}
              controls
              preload="metadata"
              src={convertFileSrc(review.audio_path)}
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
                      <span className="chunk-text">{c.text}</span>
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>
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

            {diarizeState === "running" && (
              <p className="field-hint">{dict.diarizeRunning}</p>
            )}
            {diarizeState === "failed" && (
              <p className="field-hint">{dict.diarizeFailed}</p>
            )}
            {renameable.length > 0 && (
              <>
                <h3>{dict.reviewSpeakers}</h3>
                <p className="field-hint">{dict.reviewSpeakersHint}</p>
                {renameable.map((s) => (
                  <div className="speaker-row" key={s}>
                    <span className="orig">{s}</span>
                    <input
                      type="text"
                      value={names[s] ?? s}
                      onChange={(e) =>
                        setNames({ ...names, [s]: e.target.value })
                      }
                    />
                  </div>
                ))}
              </>
            )}

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
