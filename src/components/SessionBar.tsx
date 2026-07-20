import { useEffect, useState } from "react";
import { api } from "../api";
import { IconDoc, IconRefresh } from "./Icons";
import { useSally } from "../store";

function formatElapsed(ms: number): string {
  const totalS = Math.floor(ms / 1000);
  const h = Math.floor(totalS / 3600);
  const m = Math.floor((totalS % 3600) / 60);
  const s = totalS % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

export function SessionBar() {
  const {
    dict,
    phase,
    paused,
    meetingStartedAt,
    pausedAccumMs,
    pausedSince,
    config,
    status,
    statusDetail,
    meetingEndedAt,
    warning,
    setPhase,
    setPaused,
    setReview,
    startMeetingClock,
    stopMeetingClock,
    resetMeeting,
    setStatus,
    setConfig,
  } = useSally();
  const [now, setNow] = useState(Date.now());
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [pickingSource, setPickingSource] = useState(false);
  const [audioApps, setAudioApps] = useState<string[]>([]);
  const [sourceChoice, setSourceChoice] = useState("");

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, []);

  // Clock freezes at meeting end instead of ticking forever.
  const clockNow =
    meetingEndedAt ?? (paused && pausedSince ? pausedSince : now);
  const elapsed = meetingStartedAt
    ? clockNow - meetingStartedAt - pausedAccumMs
    : 0;

  const openSourcePicker = () => {
    setError("");
    setSourceChoice(config?.capture_app ?? "");
    setPickingSource(true);
    api.listAudioApps().then(setAudioApps).catch(() => {});
  };

  const confirmSourceAndStart = async () => {
    setPickingSource(false);
    setError("");
    setBusy(true);
    try {
      if (sourceChoice !== (config?.capture_app ?? "")) {
        const updated = await api.saveSettings({ capture_app: sourceChoice });
        setConfig(updated);
      }
      resetMeeting();
      await api.startMeeting(config?.target_language);
      startMeetingClock();
      setPhase("live");
      setStatus("connecting", "");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const togglePause = async () => {
    try {
      if (paused) {
        await api.resumeMeeting();
        setPaused(false);
      } else {
        await api.pauseMeeting();
        setPaused(true);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const end = async () => {
    setConfirmEnd(false);
    setBusy(true);
    try {
      const review = await api.endMeeting();
      stopMeetingClock();
      setReview(review);
      setPhase("saved");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const openProcessing = async () => {
    // Refresh from the core in case the app state was reset; the screen
    // itself lists past meetings, so it opens either way.
    const last = await api.getLastMeeting().catch(() => null);
    if (last) setReview(last);
    setPhase("processing");
  };

  return (
    <>
      <div className="session-bar">
        <span className="elapsed">{formatElapsed(Math.max(0, elapsed))}</span>
        {phase !== "live" ? (
          <>
            <button className="btn primary" onClick={openSourcePicker} disabled={busy}>
              {dict.start}
            </button>
            <button
              className="btn"
              title={dict.processLastMeeting}
              onClick={openProcessing}
            >
              <IconDoc />
            </button>
          </>
        ) : (
          <>
            <button className="btn" onClick={togglePause}>
              {paused ? dict.resume : dict.pause}
            </button>
            <button
              className="btn danger"
              onClick={() => setConfirmEnd(true)}
              disabled={busy}
            >
              {dict.endMeeting}
            </button>
          </>
        )}
        <span className="spacer" />
        {error ? (
          <span className="error-text" title={error}>
            {error}
          </span>
        ) : (status === "reconnecting" || status === "storage-error") &&
          statusDetail ? (
          <span className="error-text" title={statusDetail}>
            {statusDetail}
          </span>
        ) : (
          warning && (
            <span className="warn-text" title={warning}>
              {warning}
            </span>
          )
        )}
      </div>
      {confirmEnd && (
        <div className="overlay">
          <div className="sheet">
            <h2>{dict.confirmEnd}</h2>
            <p>{dict.confirmEndDetail}</p>
            <div className="row end">
              <button className="btn" onClick={() => setConfirmEnd(false)}>
                {dict.cancel}
              </button>
              <button className="btn danger" onClick={end}>
                {dict.endMeeting}
              </button>
            </div>
          </div>
        </div>
      )}
      {pickingSource && (
        <div className="overlay">
          <div className="sheet">
            <h2>{dict.captureSource}</h2>
            <p className="field-hint">{dict.captureSourceHint}</p>
            <div className="row">
              <select
                style={{ flex: 1 }}
                value={sourceChoice}
                onChange={(e) => setSourceChoice(e.target.value)}
              >
                <option value="">{dict.entireSystem}</option>
                {sourceChoice && !audioApps.includes(sourceChoice) && (
                  <option value={sourceChoice}>{sourceChoice}</option>
                )}
                {audioApps.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
              <button
                className="btn compact"
                title={dict.refresh}
                onClick={() => api.listAudioApps().then(setAudioApps).catch(() => {})}
              >
                <IconRefresh />
              </button>
            </div>
            <div className="row end">
              <button className="btn" onClick={() => setPickingSource(false)}>
                {dict.cancel}
              </button>
              <button
                className="btn primary"
                onClick={confirmSourceAndStart}
                disabled={busy}
              >
                {dict.start}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
