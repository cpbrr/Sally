// First-run setup (design §6.3): interface language, API key, data folder,
// privacy disclosure, permissions note, connectivity test.

import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useState } from "react";
import { api } from "../api";
import { UiLanguage } from "../i18n";
import { useSally } from "../store";

export function SetupWizard() {
  const { dict, uiLanguage, setUiLanguage, setConfig, setPhase } = useSally();
  const [step, setStep] = useState(0);
  const [apiKey, setApiKey] = useState("");
  const [dataDir, setDataDir] = useState("");
  const [privacyAccepted, setPrivacyAccepted] = useState(false);
  const [testState, setTestState] = useState<"idle" | "running" | "ok" | "failed">("idle");
  const [error, setError] = useState("");

  const totalSteps = 6;

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
