import { useEffect, useRef, useState } from "react";
import { api, formatTimestamp } from "../api";
import { CornerTools } from "./CornerTools";
import { IconDoc, IconRefresh, IconWarning } from "./Icons";
import { useSally } from "../store";
import { useShallow } from "zustand/react/shallow";

/// Replaces the old always-visible inline error/warning text with a small
/// warning-icon button; clicking it opens a bubble below with the full
/// message, closed by clicking it again or anywhere outside.
function IssueIndicator({
  message,
  isError,
}: {
  message: string;
  isError: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener("mousedown", onClick);
    return () => window.removeEventListener("mousedown", onClick);
  }, [open]);

  useEffect(() => {
    if (!message) setOpen(false);
  }, [message]);

  if (!message) return null;

  return (
    <div className="issue-indicator" ref={ref}>
      <button
        className={`icon-btn ${isError ? "issue-error" : "issue-warn"}`}
        title={message}
        onClick={() => setOpen(!open)}
      >
        <IconWarning />
      </button>
      {open && (
        <div className={`issue-bubble ${isError ? "issue-error" : "issue-warn"}`}>
          {message}
        </div>
      )}
    </div>
  );
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
  } = useSally(
    useShallow((s) => ({
      dict: s.dict,
      phase: s.phase,
      paused: s.paused,
      meetingStartedAt: s.meetingStartedAt,
      pausedAccumMs: s.pausedAccumMs,
      pausedSince: s.pausedSince,
      config: s.config,
      status: s.status,
      statusDetail: s.statusDetail,
      meetingEndedAt: s.meetingEndedAt,
      warning: s.warning,
      setPhase: s.setPhase,
      setPaused: s.setPaused,
      setReview: s.setReview,
      startMeetingClock: s.startMeetingClock,
      stopMeetingClock: s.stopMeetingClock,
      resetMeeting: s.resetMeeting,
      setStatus: s.setStatus,
      setConfig: s.setConfig,
    }))
  );
  const [now, setNow] = useState(Date.now());
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [pickingSource, setPickingSource] = useState(false);
  const [audioApps, setAudioApps] = useState<string[]>([]);
  const [sourceChoice, setSourceChoice] = useState("");

  useEffect(() => {
    if (phase !== "live") return;
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, [phase]);

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
        <span className="elapsed">{formatTimestamp(Math.max(0, elapsed))}</span>
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
        <IssueIndicator
          message={
            error ||
            ((status === "reconnecting" || status === "storage-error") &&
              statusDetail) ||
            warning ||
            ""
          }
          isError={
            !!error ||
            ((status === "reconnecting" || status === "storage-error") &&
              !!statusDetail)
          }
        />
        <CornerTools />
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
