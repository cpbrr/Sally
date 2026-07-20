import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";
import { api } from "../api";
import {
  IconClose,
  IconGear,
  IconMinus,
  IconPin,
  IconPinOff,
  IconSpeakerOff,
  IconSpeakerOn,
} from "./Icons";
import { useSally } from "../store";

const STATUS_KEYS: Record<string, string> = {
  connecting: "statusConnecting",
  live: "statusLive",
  reconnecting: "statusReconnecting",
  paused: "statusPaused",
  ended: "statusEnded",
  idle: "statusIdle",
  "storage-error": "statusStorageError",
};

const TEXT_SCALES = [1, 1.15, 1.3, 0.9];

export function TitleBar() {
  const { dict, status, config, setConfig, showSettings, setShowSettings } =
    useSally();
  const [pinned, setPinned] = useState(config?.always_on_top ?? false);
  const [volume, setVolume] = useState(config?.readout_volume ?? 1);
  const [textScale, setTextScale] = useState(() => {
    const saved = Number(localStorage.getItem("sally.textscale"));
    return TEXT_SCALES.includes(saved) ? saved : 1;
  });

  useEffect(() => {
    setVolume(config?.readout_volume ?? 1);
  }, [config?.readout_volume]);

  // Transcript text size, applied via CSS variable and persisted locally.
  useEffect(() => {
    document.documentElement.style.setProperty(
      "--text-scale",
      String(textScale)
    );
    localStorage.setItem("sally.textscale", String(textScale));
  }, [textScale]);

  const cycleTextScale = () => {
    const i = TEXT_SCALES.indexOf(textScale);
    setTextScale(TEXT_SCALES[(i + 1) % TEXT_SCALES.length]);
  };

  const commitVolume = async (v: number) => {
    const updated = await api.setReadoutVolume(v).catch(() => null);
    if (updated) setConfig(updated);
  };

  // Apply the configured default (off unless enabled in Settings) once the
  // config loads; the pin button overrides it for this window.
  useEffect(() => {
    const v = config?.always_on_top ?? false;
    setPinned(v);
    getCurrentWindow().setAlwaysOnTop(v);
  }, [config?.always_on_top]);

  const togglePin = async () => {
    const next = !pinned;
    setPinned(next);
    await getCurrentWindow().setAlwaysOnTop(next);
  };

  const toggleReadout = async () => {
    const updated = await api.setReadout(!(config?.readout_enabled ?? false));
    setConfig(updated);
  };

  const statusKey = STATUS_KEYS[status] ?? "statusIdle";
  const statusLabel = (dict as unknown as Record<string, string>)[statusKey];

  return (
    <div className="titlebar">
      <div
        className="drag-region"
        onMouseDown={(e) => {
          if (e.buttons === 1 && e.detail !== 2) {
            getCurrentWindow().startDragging();
          }
        }}
      >
        <span className="app-name">{dict.appName}</span>
        <span className={`status-dot ${status}`} />
        <span className="status-text">{statusLabel}</span>
      </div>
      <button
        className={`icon-btn ${config?.readout_enabled ? "active" : ""}`}
        title={config?.readout_enabled ? dict.readoutOff : dict.readoutOn}
        onClick={toggleReadout}
      >
        {config?.readout_enabled ? <IconSpeakerOn /> : <IconSpeakerOff />}
      </button>
      <input
        className="volume-slider"
        type="range"
        min={0}
        max={100}
        value={Math.round(volume * 100)}
        title={dict.readoutVolume}
        onChange={(e) => setVolume(Number(e.target.value) / 100)}
        onPointerUp={() => commitVolume(volume)}
        onKeyUp={() => commitVolume(volume)}
      />
      <button
        className="icon-btn text-size-btn"
        title={dict.textSize}
        onClick={cycleTextScale}
      >
        A
      </button>
      <button
        className={`icon-btn ${pinned ? "active" : ""}`}
        title={pinned ? dict.unpin : dict.pin}
        onClick={togglePin}
      >
        {pinned ? <IconPin /> : <IconPinOff />}
      </button>
      <button
        className={`icon-btn ${showSettings ? "active" : ""}`}
        title={dict.settings}
        onClick={() => setShowSettings(!showSettings)}
      >
        <IconGear />
      </button>
      <button
        className="icon-btn"
        title="Minimize"
        onClick={() => getCurrentWindow().minimize()}
      >
        <IconMinus />
      </button>
      <button
        className="icon-btn"
        title="Close"
        onClick={() => getCurrentWindow().close()}
      >
        <IconClose />
      </button>
    </div>
  );
}
