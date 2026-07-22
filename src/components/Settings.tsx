import { getVersion } from "@tauri-apps/api/app";
import { open } from "@tauri-apps/plugin-dialog";
import { Dispatch, SetStateAction, useEffect, useState } from "react";
import { api, AudioDevices } from "../api";
import {
  DEFAULT_CLEANUP_MODEL,
  DEFAULT_LIVE_MODEL,
  Dict,
  TARGET_LANGUAGES,
  UiLanguage,
} from "../i18n";
import { Phase, useSally } from "../store";
import { useShallow } from "zustand/react/shallow";
import {
  isTranslucent,
  loadLevel,
  setLevel,
  setTranslucent,
} from "../transparency";
import { IconChevron, IconEye, IconEyeOff, IconRefresh, IconReset } from "./Icons";
import { isMac } from "../platform";

type SettingsForm = {
  ui_language: string;
  target_language: string;
  data_dir: string;
  mic_device: string;
  system_device: string;
  capture_app: string;
  live_model: string;
  cleanup_model: string;
  mac_capture_method: string;
  split_line_count: number;
};

function AdvancedSettings({
  dict,
  showAdvanced,
  onToggleAdvanced,
  form,
  setForm,
  phase,
  apiKey,
  onApiKeyChange,
  showKey,
  onToggleShowKey,
}: {
  dict: Dict;
  showAdvanced: boolean;
  onToggleAdvanced: () => void;
  form: SettingsForm;
  setForm: Dispatch<SetStateAction<SettingsForm>>;
  phase: Phase;
  apiKey: string;
  onApiKeyChange: (v: string) => void;
  showKey: boolean;
  onToggleShowKey: () => void;
}) {
  const [version, setVersion] = useState("");

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
  }, []);

  return (
    <>
      <button className="btn advanced-toggle" onClick={onToggleAdvanced}>
        <IconChevron open={showAdvanced} /> {dict.advanced}
      </button>

      {showAdvanced && (
        <>
          {isMac() && (
            <label>
              {dict.macCaptureMethod}
              <select
                value={form.mac_capture_method}
                onChange={(e) =>
                  setForm({ ...form, mac_capture_method: e.target.value })
                }
              >
                <option value="auto">{dict.macCaptureAuto}</option>
                <option value="tap">{dict.macCaptureTap}</option>
                <option value="screencapturekit">
                  {dict.macCaptureSCK}
                </option>
              </select>
            </label>
          )}
          {isMac() && (
            <p className="field-hint">{dict.macCaptureMethodHint}</p>
          )}

          <label>
            {dict.setupApiKey}
            <div className="row">
              <input
                type={showKey ? "text" : "password"}
                style={{ flex: 1 }}
                value={apiKey}
                onChange={(e) => onApiKeyChange(e.target.value)}
              />
              <button
                className="btn compact"
                title={showKey ? "Hide" : "Show"}
                onClick={onToggleShowKey}
              >
                {showKey ? <IconEyeOff /> : <IconEye />}
              </button>
            </div>
          </label>

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
            {dict.splitLineCount}
            <input
              type="number"
              min={0}
              max={20}
              value={form.split_line_count}
              onChange={(e) =>
                setForm({
                  ...form,
                  split_line_count: Math.max(0, Number(e.target.value)),
                })
              }
            />
          </label>
          <p className="field-hint">{dict.splitLineCountHint}</p>

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
                <IconReset />
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
                onChange={(e) =>
                  setForm({ ...form, cleanup_model: e.target.value })
                }
              />
              <button
                className="btn compact"
                title={dict.revertDefault}
                onClick={() =>
                  setForm({ ...form, cleanup_model: DEFAULT_CLEANUP_MODEL })
                }
              >
                <IconReset />
              </button>
            </div>
          </label>

          {version && <p className="field-hint">{dict.version} {version}</p>}
        </>
      )}
    </>
  );
}

export function Settings() {
  const { dict, config, setConfig, setUiLanguage, setShowSettings, phase } =
    useSally(
      useShallow((s) => ({
        dict: s.dict,
        config: s.config,
        setConfig: s.setConfig,
        setUiLanguage: s.setUiLanguage,
        setShowSettings: s.setShowSettings,
        phase: s.phase,
      }))
    );
  const [devices, setDevices] = useState<AudioDevices>({ inputs: [], outputs: [] });
  const [audioApps, setAudioApps] = useState<string[]>([]);
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [form, setForm] = useState({
    ui_language: config?.ui_language ?? "en",
    target_language: config?.target_language ?? "Vietnamese",
    data_dir: config?.data_dir ?? "",
    mic_device: config?.mic_device ?? "",
    system_device: config?.system_device ?? "",
    capture_app: config?.capture_app ?? "",
    live_model: config?.live_model ?? "",
    cleanup_model: config?.cleanup_model ?? "",
    mac_capture_method: config?.mac_capture_method ?? "auto",
    split_line_count: config?.split_line_count ?? 3,
  });
  const [error, setError] = useState("");
  const [translucent, setTranslucentState] = useState(isTranslucent());
  const [alpha, setAlpha] = useState(loadLevel());

  const refreshApps = () => {
    api.listAudioApps().then(setAudioApps).catch(() => {});
  };

  useEffect(() => {
    api.listAudioDevices().then(setDevices).catch(() => {});
    // The Core Audio tap enumeration (tried first on macOS, same as
    // Windows' session walk) is permission-free, so fetching eagerly here
    // costs nothing on either platform.
    refreshApps();
    // Show the stored key so the box never looks empty after saving.
    api.getApiKey().then(setApiKey).catch(() => {});
  }, []);

  // Re-fetch when the window regains focus, so an app that started playing
  // audio while the user was elsewhere (or a permission just granted in
  // System Settings) shows up without a manual refresh click.
  useEffect(() => {
    const onFocus = () => refreshApps();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
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
      // The picked device/app is persisted above, but a running meeting
      // keeps its own copy of config — nudge it live too instead of
      // waiting for the next meeting to pick the change up.
      if (phase === "live") {
        if (form.mic_device !== config?.mic_device) {
          await api.switchMic(form.mic_device).catch(() => {});
        }
        if (form.capture_app !== config?.capture_app) {
          await api.switchCaptureApp(form.capture_app).catch(() => {});
        }
      }
      setShowSettings(false);
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleTranslucent = (on: boolean) => {
    setTranslucentState(on);
    setTranslucent(on);
    if (on) setAlpha(loadLevel());
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
          {dict.captureSource}
          <div className="row">
            <select
              style={{ flex: 1 }}
              value={form.capture_app}
              onChange={(e) => setForm({ ...form, capture_app: e.target.value })}
            >
              <option value="">{dict.entireSystem}</option>
              {form.capture_app && !audioApps.includes(form.capture_app) && (
                <option value={form.capture_app}>{form.capture_app}</option>
              )}
              {audioApps.map((a) => (
                <option key={a} value={a}>
                  {a}
                </option>
              ))}
            </select>
            <button className="btn compact" title={dict.refresh} onClick={refreshApps}>
              <IconRefresh />
            </button>
          </div>
        </label>
        <p className="field-hint">{dict.captureSourceHint}</p>

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
        {!isMac() && <p className="field-hint">{dict.micDeviceHint}</p>}

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

        <label>
          {dict.setupDataFolder}
          <div className="row">
            <input type="text" value={form.data_dir} readOnly style={{ flex: 1 }} />
            <button className="btn" onClick={pickFolder} disabled={phase === "live"}>
              {dict.chooseFolder}
            </button>
          </div>
        </label>

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

        <AdvancedSettings
          dict={dict}
          showAdvanced={showAdvanced}
          onToggleAdvanced={() => setShowAdvanced(!showAdvanced)}
          form={form}
          setForm={setForm}
          phase={phase}
          apiKey={apiKey}
          onApiKeyChange={setApiKey}
          showKey={showKey}
          onToggleShowKey={() => setShowKey(!showKey)}
        />

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
