// Post-meeting flow. SavedPopup: small confirmation right after End
// Meeting. ProcessingScreen: full-window pre-processing menu (not an
// overlay) — meeting name, speaker renames, export/AI options, then a
// success state with an Open Markdown button.

import { openPath } from "@tauri-apps/plugin-opener";
import { useState } from "react";
import { api } from "../api";
import { useSally } from "../store";

export function SavedPopup() {
  const { dict, setPhase } = useSally();
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

export function ProcessingScreen() {
  const { dict, review, setReview, setPhase } = useSally();
  const [meetingTitle, setMeetingTitle] = useState("");
  const [names, setNames] = useState<Record<string, string>>(() =>
    Object.fromEntries((review?.speakers ?? []).map((s) => [s, s]))
  );
  const [includeTimestamps, setIncludeTimestamps] = useState(true);
  const [exportCopy, setExportCopy] = useState(false);
  const [aiCleanup, setAiCleanup] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [resultPath, setResultPath] = useState<string | null>(null);

  if (!review) return null;

  const renameable = review.speakers.filter(
    (s) => s !== "You" && s !== "Meeting"
  );

  const run = async () => {
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
      if (exportCopy) {
        await api.exportWithoutTimestamps();
      }
      if (aiCleanup) {
        setResultPath(await api.cleanAndSummarize(includeTimestamps));
      } else {
        setResultPath(updated.raw_path);
      }
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
          <button
            className="btn compact"
            title={dict.openRawFolder}
            onClick={() => openPath(review.raw_dir)}
          >
            📁
          </button>
        </div>

        {resultPath === null ? (
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
                checked={exportCopy}
                onChange={(e) => setExportCopy(e.target.checked)}
              />
              {dict.exportNoTimestamps}
            </label>

            <label className="check">
              <input
                type="checkbox"
                checked={aiCleanup}
                onChange={(e) => setAiCleanup(e.target.checked)}
              />
              {dict.aiCleanup}
            </label>
            <p className="field-hint">{dict.aiCleanupHint}</p>

            {aiCleanup && (
              <label className="check">
                <input
                  type="checkbox"
                  checked={includeTimestamps}
                  onChange={(e) => setIncludeTimestamps(e.target.checked)}
                />
                {dict.includeTimestamps}
              </label>
            )}

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
        ) : (
          <>
            <p className="ok-text">{dict.processingSuccess}</p>
            <p className="field-hint">{resultPath}</p>
            <div className="row end">
              <button className="btn" onClick={() => setPhase("idle")}>
                {dict.backToApp}
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
      </div>
    </div>
  );
}
