// End-of-meeting review (design §6.4): speaker rename/merge, exports,
// optional Clean & Summarize. The raw transcript is already preserved.

import { openPath } from "@tauri-apps/plugin-opener";
import { useState } from "react";
import { api } from "../api";
import { useSally } from "../store";

export function ReviewScreen() {
  const { dict, review, setReview, setPhase, resetMeeting } = useSally();
  const [names, setNames] = useState<Record<string, string>>(() =>
    Object.fromEntries((review?.speakers ?? []).map((s) => [s, s]))
  );
  const [meetingTitle, setMeetingTitle] = useState("");
  const [includeTimestamps, setIncludeTimestamps] = useState(false);
  const [exportedPath, setExportedPath] = useState("");
  const [polishedPath, setPolishedPath] = useState("");
  const [cleaning, setCleaning] = useState(false);
  const [error, setError] = useState("");

  if (!review) return null;

  const applyNames = async () => {
    setError("");
    try {
      const renames = Object.fromEntries(
        Object.entries(names).filter(([o, n]) => n.trim() && n.trim() !== o)
      );
      const updated = await api.applyReview(
        renames,
        meetingTitle.trim() || undefined
      );
      setReview(updated);
      setNames(Object.fromEntries(updated.speakers.map((s) => [s, s])));
    } catch (e) {
      setError(String(e));
    }
  };

  const exportCopy = async () => {
    setError("");
    try {
      if (includeTimestamps) {
        // Raw already contains timestamps; the copy is only needed for the
        // timestamp-free variant. With timestamps, open the raw directly.
        setExportedPath(review.raw_path);
      } else {
        setExportedPath(await api.exportWithoutTimestamps());
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const clean = async () => {
    setError("");
    setCleaning(true);
    try {
      setPolishedPath(await api.cleanAndSummarize(includeTimestamps));
    } catch (e) {
      setError(String(e));
    } finally {
      setCleaning(false);
    }
  };

  const done = () => {
    resetMeeting();
    setPhase("idle");
  };

  return (
    <div className="overlay">
      <div className="sheet">
        <h2>{dict.reviewTitle}</h2>

        <label>
          {dict.meetingName}
          <input
            type="text"
            value={meetingTitle}
            onChange={(e) => setMeetingTitle(e.target.value)}
            placeholder="Untitled meeting"
          />
        </label>

        <h3>{dict.reviewSpeakers}</h3>
        <p className="field-hint">{dict.reviewSpeakersHint}</p>
        {review.speakers
          .filter((s) => s !== "You" && s !== "Meeting")
          .map((s) => (
            <div className="speaker-row" key={s}>
              <span className="orig">{s}</span>
              <input
                type="text"
                value={names[s] ?? s}
                onChange={(e) => setNames({ ...names, [s]: e.target.value })}
              />
            </div>
          ))}
        <div className="row">
          <button className="btn" onClick={applyNames}>
            {dict.applyNames}
          </button>
        </div>

        <label className="check">
          <input
            type="checkbox"
            checked={includeTimestamps}
            onChange={(e) => setIncludeTimestamps(e.target.checked)}
          />
          {dict.includeTimestamps}
        </label>

        <div className="row">
          <button className="btn" onClick={() => openPath(review.raw_path)}>
            {dict.openRaw}
          </button>
          <button className="btn" onClick={exportCopy}>
            {dict.exportCopy}
          </button>
        </div>
        {exportedPath && (
          <p className="ok-text">
            {dict.exported} {exportedPath}
          </p>
        )}

        <div className="row">
          <button className="btn primary" onClick={clean} disabled={cleaning}>
            {cleaning ? dict.cleaning : dict.cleanAndSummarize}
          </button>
          {polishedPath && (
            <button className="btn" onClick={() => openPath(polishedPath)}>
              {dict.openPolished}
            </button>
          )}
        </div>

        {error && <p className="error-text">{error}</p>}

        <div className="row end">
          <button className="btn primary" onClick={done}>
            {dict.done}
          </button>
        </div>
      </div>
    </div>
  );
}
