import { useEffect, useState } from "react";
import { api } from "../api";
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
    setPhase,
    setPaused,
    setReview,
    startMeetingClock,
    resetMeeting,
    setStatus,
  } = useSally();
  const [now, setNow] = useState(Date.now());
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, []);

  const elapsed = meetingStartedAt
    ? (paused && pausedSince ? pausedSince : now) -
      meetingStartedAt -
      pausedAccumMs
    : 0;

  const start = async () => {
    setError("");
    setBusy(true);
    try {
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
      setReview(review);
      setPhase("review");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="session-bar">
        <span className="elapsed">{formatElapsed(Math.max(0, elapsed))}</span>
        {phase !== "live" ? (
          <button className="btn primary" onClick={start} disabled={busy}>
            {dict.start}
          </button>
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
        {error && <span className="error-text">{error}</span>}
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
    </>
  );
}
