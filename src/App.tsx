import { useEffect, useRef, useState } from "react";
import { api, onEntry, onPartial, onStatus, onWarning } from "./api";
import { Panels } from "./components/Panels";
import { ProcessingScreen, SavedPopup } from "./components/Processing";
import { SessionBar } from "./components/SessionBar";
import { Settings } from "./components/Settings";
import { SetupWizard } from "./components/SetupWizard";
import { TitleBar } from "./components/TitleBar";
import { useSally } from "./store";
import { initTransparency } from "./transparency";

function RecoveryPrompt() {
  const { dict, setPendingRecoveries } = useSally();
  const [recovered, setRecovered] = useState<string[] | null>(null);
  const [error, setError] = useState("");

  const recover = async () => {
    try {
      setRecovered(await api.recoverMeetings());
    } catch (e) {
      setError(String(e));
    }
  };

  const close = () => setPendingRecoveries(0);

  return (
    <div className="overlay">
      <div className="sheet">
        <h2>{dict.recoveryTitle}</h2>
        {recovered === null ? (
          <>
            <p>{dict.recoveryBody}</p>
            {error && <p className="error-text">{error}</p>}
            <div className="row end">
              <button className="btn" onClick={close}>
                {dict.discard}
              </button>
              <button className="btn primary" onClick={recover}>
                {dict.recover}
              </button>
            </div>
          </>
        ) : (
          <>
            {recovered.map((p) => (
              <p className="ok-text" key={p}>
                {dict.recovered} {p}
              </p>
            ))}
            <div className="row end">
              <button className="btn primary" onClick={close}>
                {dict.done}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default function App() {
  const {
    phase,
    pendingRecoveries,
    showSettings,
    setPhase,
    setConfig,
    setPendingRecoveries,
    setStatus,
    setWarning,
    addEntry,
    setPartial,
    setPaused,
  } = useSally();
  const booted = useRef(false);

  useEffect(() => {
    if (booted.current) return;
    booted.current = true;

    initTransparency();

    const unlisteners: Array<Promise<() => void>> = [
      onStatus((s) => {
        setStatus(s.state, s.detail);
        if (s.state === "paused") setPaused(true);
        if (s.state === "live") setPaused(false);
      }),
      onEntry((e) => addEntry(e)),
      onPartial((p) => setPartial(p)),
      onWarning((w) => setWarning(w)),
    ];

    api
      .getBootInfo()
      .then((info) => {
        setConfig(info.config);
        setPendingRecoveries(info.pending_recoveries);
        setPhase(info.needs_setup ? "setup" : "idle");
      })
      .catch(() => setPhase("setup"));

    return () => {
      unlisteners.forEach((p) => p.then((un) => un()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="app">
      <TitleBar />
      {phase === "processing" ? (
        <ProcessingScreen />
      ) : (
        <>
          <Panels />
          <SessionBar />
        </>
      )}
      {phase === "setup" && <SetupWizard />}
      {phase === "saved" && <SavedPopup />}
      {showSettings && <Settings />}
      {phase === "idle" && pendingRecoveries > 0 && <RecoveryPrompt />}
    </div>
  );
}
