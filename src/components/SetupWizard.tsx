// First-run setup (design §6.3): interface language, API key, data folder,
// privacy disclosure, permissions note, connectivity test.

import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { TARGET_LANGUAGES, UiLanguage } from "../i18n";
import { useSally } from "../store";
import { useShallow } from "zustand/react/shallow";

export function SetupWizard() {
  const { dict, uiLanguage, setUiLanguage, setConfig, setPhase } = useSally(
    useShallow((s) => ({
      dict: s.dict,
      uiLanguage: s.uiLanguage,
      setUiLanguage: s.setUiLanguage,
      setConfig: s.setConfig,
      setPhase: s.setPhase,
    }))
  );
  const [step, setStep] = useState(0);
  const [targetLanguage, setTargetLanguage] = useState("Vietnamese");
  const [apiKey, setApiKey] = useState("");
  const [dataDir, setDataDir] = useState("");
  const [privacyAccepted, setPrivacyAccepted] = useState(false);
  const [testState, setTestState] = useState<"idle" | "running" | "ok" | "failed">("idle");
  const [error, setError] = useState("");
  const micPermissionRequested = useRef(false);

  const totalSteps = 6;

  // Trigger the OS mic-permission prompt right here, on the dedicated
  // "permissions" step — deliberately ahead of the Screen Recording
  // prompt, which only fires later once the user reaches the main screen
  // (App.tsx). Requesting both around the same moment (previously: both
  // deferred to the first meeting) let one appear behind the other and
  // get missed.
  useEffect(() => {
    if (step === 4 && !micPermissionRequested.current) {
      micPermissionRequested.current = true;
      api.requestMicPermission().catch(() => {});
    }
  }, [step]);

  const pickFolder = async () => {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") setDataDir(dir);
  };

  const saveAndTest = async () => {
    setError("");
    setTestState("running");
    try {
      const cfg = await api.saveSettings({
        data_dir: dataDir,
        api_key: apiKey,
        ui_language: uiLanguage,
        target_language: targetLanguage,
      });
      setConfig(cfg);
      await api.testConnectivity();
      setTestState("ok");
    } catch (e) {
      setTestState("failed");
      setError(String(e));
    }
  };

  const finish = () => setPhase("idle");

  const canNext = () => {
    switch (step) {
      case 1:
        return apiKey.trim().length > 0;
      case 2:
        return dataDir.trim().length > 0;
      case 3:
        return privacyAccepted;
      case 5:
        return testState === "ok";
      default:
        return true;
    }
  };

  return (
    <div className="overlay">
      <div className="sheet">
        <div className="setup-steps">
          {step + 1} / {totalSteps}
        </div>
        {step === 0 && (
          <>
            <h2>{dict.setupTitle}</h2>
            <p>{dict.setupIntro}</p>
            <label>
              {dict.setupLanguage}
              <select
                value={uiLanguage}
                onChange={(e) => setUiLanguage(e.target.value as UiLanguage)}
              >
                <option value="en">English</option>
                <option value="vi">Tiếng Việt</option>
                <option value="ja">日本語</option>
              </select>
            </label>
            <label>
              {dict.targetLanguage}
              <select
                value={targetLanguage}
                onChange={(e) => setTargetLanguage(e.target.value)}
              >
                {TARGET_LANGUAGES.map((l) => (
                  <option key={l} value={l}>
                    {l}
                  </option>
                ))}
              </select>
            </label>
          </>
        )}
        {step === 1 && (
          <>
            <h2>{dict.setupApiKey}</h2>
            <label>
              {dict.setupApiKey}
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="AIza…"
              />
            </label>
            <p className="field-hint">{dict.setupApiKeyHint}</p>
            <button
              className="btn"
              onClick={() => openUrl("https://aistudio.google.com/api-keys")}
            >
              {dict.getApiKeyLink}
            </button>
          </>
        )}
        {step === 2 && (
          <>
            <h2>{dict.setupDataFolder}</h2>
            <div className="row">
              <input type="text" value={dataDir} readOnly style={{ flex: 1 }} />
              <button className="btn" onClick={pickFolder}>
                {dict.chooseFolder}
              </button>
            </div>
          </>
        )}
        {step === 3 && (
          <>
            <h2>{dict.setupPrivacyTitle}</h2>
            <p>{dict.setupPrivacy}</p>
            <label className="check">
              <input
                type="checkbox"
                checked={privacyAccepted}
                onChange={(e) => setPrivacyAccepted(e.target.checked)}
              />
              {dict.setupPrivacyAccept}
            </label>
          </>
        )}
        {step === 4 && (
          <>
            <h2>{dict.setupPermissions}</h2>
            <p>{dict.setupPermissionsHint}</p>
          </>
        )}
        {step === 5 && (
          <>
            <h2>{dict.setupTest}</h2>
            <div className="row">
              <button
                className="btn primary"
                onClick={saveAndTest}
                disabled={testState === "running"}
              >
                {testState === "running" ? dict.setupTestRunning : dict.setupTest}
              </button>
              {testState === "ok" && <span className="ok-text">{dict.setupTestOk}</span>}
              {testState === "failed" && (
                <span className="error-text">{dict.setupTestFailed}</span>
              )}
            </div>
            {error && <p className="error-text">{error}</p>}
          </>
        )}
        <div className="row end">
          {step > 0 && (
            <button className="btn" onClick={() => setStep(step - 1)}>
              {dict.back}
            </button>
          )}
          {step < totalSteps - 1 ? (
            <button
              className="btn primary"
              disabled={!canNext()}
              onClick={() => setStep(step + 1)}
            >
              {dict.next}
            </button>
          ) : (
            <button className="btn primary" disabled={!canNext()} onClick={finish}>
              {dict.finish}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
