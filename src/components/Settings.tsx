import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { api, AudioDevices } from "../api";
import { TARGET_LANGUAGES, UiLanguage } from "../i18n";
import { useSally } from "../store";

export function Settings() {
  const { dict, config, setConfig, setUiLanguage, setShowSettings, phase } =
    useSally();
  const [devices, setDevices] = useState<AudioDevices>({ inputs: [], outputs: [] });
  const [apiKey, setApiKey] = useState("");
  const [form, setForm] = useState({
    ui_language: config?.ui_language ?? "en",
    target_language: config?.target_language ?? "Vietnamese",
    data_dir: config?.data_dir ?? "",
    mic_device: config?.mic_device ?? "",
    system_device: config?.system_device ?? "",
    diarization_enabled: config?.diarization_enabled ?? true,
    always_on_top: config?.always_on_top ?? true,
    readout_enabled: config?.readout_enabled ?? false,
    live_model: config?.live_model ?? "",
    cleanup_model: config?.cleanup_model ?? "",
  });
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    api.listAudioDevices().then(setDevices).catch(() => {});
  }, []);

  const pickFolder = async () => {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") setForm({ ...form, data_dir: dir });
  };

  const save = async () => {
    setError("");
    setSaved(false);
    try {
      const updated = await api.saveSettings({
        ...form,
        api_key: apiKey.trim() || undefined,
      });
      setConfig(updated);
      setUiLanguage(form.ui_language as UiLanguage);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    }
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
          <input
            type="password"
            value={apiKey}
            placeholder={config?.has_api_key ? "••••••••" : ""}
            onChange={(e) => setApiKey(e.target.value)}
          />
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
            checked={form.diarization_enabled}
            onChange={(e) =>
              setForm({ ...form, diarization_enabled: e.target.checked })
            }
          />
          {dict.diarization}
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

        <label>
          {dict.liveModel}
          <input
            type="text"
            value={form.live_model}
            onChange={(e) => setForm({ ...form, live_model: e.target.value })}
          />
        </label>

        <label>
          {dict.cleanupModel}
          <input
            type="text"
            value={form.cleanup_model}
            onChange={(e) => setForm({ ...form, cleanup_model: e.target.value })}
          />
        </label>

        {error && <p className="error-text">{error}</p>}
        {saved && <p className="ok-text">{dict.saved}</p>}

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
