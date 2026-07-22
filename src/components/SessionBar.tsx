import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, formatTimestamp } from "../api";
import { CornerTools } from "./CornerTools";
import { IconDoc, IconMic, IconMicOff, IconRefresh, IconWarning } from "./Icons";
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
    micLost,
    setPhase,
    setPaused,
    setReview,
    startMeetingClock,
    stopMeetingClock,
    resetMeeting,
    setStatus,
    setConfig,
    setMicLost,
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
      micLost: s.micLost,
      setPhase: s.setPhase,
      setPaused: s.setPaused,
      setReview: s.setReview,
      startMeetingClock: s.startMeetingClock,
      stopMeetingClock: s.stopMeetingClock,
      resetMeeting: s.resetMeeting,
      setStatus: s.setStatus,
      setConfig: s.setConfig,
      setMicLost: s.setMicLost,
    }))
  );
  const [now, setNow] = useState(Date.now());
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [pickingSource, setPickingSource] = useState(false);
  const [audioApps, setAudioApps] = useState<string[]>([]);
  const [sourceChoice, setSourceChoice] = useState("");
  const [micInputs, setMicInputs] = useState<string[]>([]);
  const [micChoice, setMicChoice] = useState("");
  const [micSwitching, setMicSwitching] = useState(false);
  const [micError, setMicError] = useState("");
  const [micMuted, setMicMuted] = useState(false);

  const refreshMicDevices = () =>
    api.listAudioDevices().then((d) => setMicInputs(d.inputs)).catch(() => {});

  useEffect(() => {
    if (!micLost) return;
    setMicError("");
    setMicChoice(config?.mic_device ?? "");
    refreshMicDevices();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [micLost]);

  const switchMic = async () => {
    setMicSwitching(true);
    setMicError("");
    try {
      const updated = await api.switchMic(micChoice);
      setConfig(updated);
      // micLost flips to false once sally://mic-recovered arrives — no
      // need to close the prompt here; if the new device also fails to
      // capture, it stays open with the error below instead.
    } catch (e) {
      setMicError(String(e));
    } finally {
      setMicSwitching(false);
    }
  };

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
    // macOS: listing prefers the Core Audio tap (permission-free) and only
    // falls back to ScreenCaptureKit — which does need Screen Recording —
    // when the tap finds nothing, so eager fetch is safe on both platforms.
    api.listAudioApps().then(setAudioApps).catch(() => {});
  };

  // If the tap fallback did need Screen Recording and the user just
  // granted it in System Settings, they return to Sally by refocusing the
  // window — refresh right then instead of waiting for a manual reload.
  useEffect(() => {
    if (!pickingSource) return;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) {
          api.listAudioApps().then(setAudioApps).catch(() => {});
        }
      })
      .then((un) => {
        unlisten = un;
      });
    return () => unlisten?.();
  }, [pickingSource]);

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
      if (micMuted) {
        await api.setMicMuted(true).catch(() => {});
      }
      startMeetingClock();
      setPhase("live");
      setStatus("connecting", "");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Available before a meeting starts too (pre-set like a "join muted"
  // choice) — applied live below once startMeeting succeeds. During a live
  // meeting it toggles immediately.
  const toggleMicMuted = async () => {
    const next = !micMuted;
    setMicMuted(next);
    if (phase === "live") {
      try {
        await api.setMicMuted(next);
      } catch (e) {
        setError(String(e));
      }
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
      // Backend resets unmuted per meeting; mirror that for the next one.
      setMicMuted(false);
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
        <button
          className={`btn ${micMuted ? "danger" : ""}`}
          title={micMuted ? dict.unmuteMic : dict.muteMic}
          onClick={toggleMicMuted}
        >
          {micMuted ? <IconMicOff /> : <IconMic />}
        </button>
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
      {micLost && (
        <div className="overlay">
          <div className="sheet">
            <h2>{dict.micLostTitle}</h2>
            <p className="field-hint">{dict.micLostHint}</p>
            {micError && <p className="error-text">{micError}</p>}
            <div className="row">
              <select
                style={{ flex: 1 }}
                value={micChoice}
                onChange={(e) => setMicChoice(e.target.value)}
              >
                {micChoice && !micInputs.includes(micChoice) && (
                  <option value={micChoice}>{micChoice}</option>
                )}
                {micInputs.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </select>
              <button
                className="btn compact"
                title={dict.refresh}
                onClick={refreshMicDevices}
              >
                <IconRefresh />
              </button>
            </div>
            <div className="row end">
              <button className="btn" onClick={() => setMicLost(false)}>
                {dict.cancel}
              </button>
              <button
                className="btn primary"
                onClick={switchMic}
                disabled={micSwitching || !micChoice}
              >
                {dict.switchMic}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
