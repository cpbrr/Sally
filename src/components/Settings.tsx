import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { api, AudioDevices } from "../api";
import {
  DEFAULT_CLEANUP_MODEL,
  DEFAULT_LIVE_MODEL,
  TARGET_LANGUAGES,
  UiLanguage,
} from "../i18n";
import { useSally } from "../store";
import {
  isTranslucent,
  loadLevel,
  setLevel,
  setTranslucent,
} from "../transparency";

export function Settings() {
  const { dict, config, setConfig, setUiLanguage, setShowSettings, phase } =
    useSally();
  const [devices, setDevices] = useState<AudioDevices>({ inputs: [], outputs: [] });
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [form, setForm] = useState({
    ui_language: config?.ui_language ?? "en",
    target_language: config?.target_language ?? "Vietnamese",
    data_dir: config?.data_dir ?? "",
    mic_device: config?.mic_device ?? "",
    system_device: config?.system_device ?? "",
    always_on_top: config?.always_on_top ?? false,
    readout_enabled: config?.readout_enabled ?? false,
    live_model: config?.live_model ?? "",
    cleanup_model: config?.cleanup_model ?? "",
  });
  const [error, setError] = useState("");
  const [translucent, setTranslucentState] = useState(isTranslucent());
  const [alpha, setAlpha] = useState(loadLevel());

  useEffect(() => {
    api.listAudioDevices().then(setDevices).catch(() => {});
    // Show the stored key so the box never looks empty after saving.
    api.getApiKey().then(setApiKey).catch(() => {});
  }, []);

  // Close on Escape.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowSettings(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setShowSettings]);

  const pickFolder = async () => {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") setForm({ ...form, data_dir: dir });
  };

  const save = async () => {
    setError("");
    try {
      const updated = await api.saveSettings({
        ...form,
        api_key: apiKey.trim() || undefined,
      });
      setConfig(updated);
      setUiLanguage(form.ui_language as UiLanguage);
      setShowSettings(false);
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleTranslucent = (on: boolean) => {
    setTranslucentState(on);
    setTranslucent(on);
  };

  const changeAlpha = (v: number) => {
    setAlpha(v);
    setLevel(v);
  };

  return (
    <div className="overlay">
      <div className="sheet">
        <h2>{dict.settingsTitle}</h2>

        <label>
          {dict.setupLanguage}
          <select
            value={form.ui_language}
            onChange={(e) => setForm({ ...form, ui_language: e.target.value })}
          >
            <option value="en">English</option>
            <option value="vi">Tiếng Việt</option>
          </select>
        </label>

        <label>
          {dict.targetLanguage}
          <select
            value={form.target_language}
            disabled={phase === "live"}
            onChange={(e) =>
              setForm({ ...form, target_language: e.target.value })
            }
          >
            {TARGET_LANGUAGES.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </label>

        <label>
          {dict.setupApiKey}
          <div className="row">
            <input
              type={showKey ? "text" : "password"}
              style={{ flex: 1 }}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
            />
            <button
              className="btn compact"
              title={showKey ? "Hide" : "Show"}
              onClick={() => setShowKey(!showKey)}
            >
              {showKey ? "🙈" : "👁"}
            </button>
          </div>
        </label>

        <label>
          {dict.setupDataFolder}
          <div className="row">
            <input type="text" value={form.data_dir} readOnly style={{ flex: 1 }} />
            <button className="btn" onClick={pickFolder} disabled={phase === "live"}>
              {dict.chooseFolder}
            </button>
          </div>
        </label>

        <label>
          {dict.micDevice}
          <select
            value={form.mic_device}
            onChange={(e) => setForm({ ...form, mic_device: e.target.value })}
          >
            <option value="">{dict.systemDefault}</option>
            {devices.inputs.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </label>

        <label>
          {dict.systemDevice}
          <select
            value={form.system_device}
            onChange={(e) => setForm({ ...form, system_device: e.target.value })}
          >
            <option value="">{dict.systemDefault}</option>
            {devices.outputs.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </label>

        <label className="check">
          <input
            type="checkbox"
            checked={form.always_on_top}
            onChange={(e) => setForm({ ...form, always_on_top: e.target.checked })}
          />
          {dict.alwaysOnTopDefault}
        </label>

        <label className="check">
          <input
            type="checkbox"
            checked={form.readout_enabled}
            onChange={(e) =>
              setForm({ ...form, readout_enabled: e.target.checked })
            }
          />
          {dict.readoutSetting}
        </label>
        <p className="field-hint">{dict.readoutHint}</p>

        <label className="check">
          <input
            type="checkbox"
            checked={translucent}
            onChange={(e) => toggleTranslucent(e.target.checked)}
          />
          {dict.translucent}
        </label>
        {translucent && (
          <label>
            {dict.transparency} ({alpha}%)
            <input
              type="range"
              min={40}
              max={100}
              value={alpha}
              onChange={(e) => changeAlpha(Number(e.target.value))}
            />
          </label>
        )}

        <label>
          {dict.liveModel}
          <div className="row">
            <input
              type="text"
              style={{ flex: 1 }}
              value={form.live_model}
              onChange={(e) => setForm({ ...form, live_model: e.target.value })}
            />
            <button
              className="btn compact"
              title={dict.revertDefault}
              onClick={() => setForm({ ...form, live_model: DEFAULT_LIVE_MODEL })}
            >
              ↺
            </button>
          </div>
        </label>

        <label>
          {dict.cleanupModel}
          <div className="row">
            <input
              type="text"
              style={{ flex: 1 }}
              value={form.cleanup_model}
              onChange={(e) => setForm({ ...form, cleanup_model: e.target.value })}
            />
            <button
              className="btn compact"
              title={dict.revertDefault}
              onClick={() =>
                setForm({ ...form, cleanup_model: DEFAULT_CLEANUP_MODEL })
              }
            >
              ↺
            </button>
          </div>
        </label>

        {error && <p className="error-text">{error}</p>}

        <div className="row end">
          <button className="btn" onClick={() => setShowSettings(false)}>
            {dict.cancel}
          </button>
          <button className="btn primary" onClick={save}>
            {dict.save}
          </button>
        </div>
      </div>
    </div>
  );
}
