import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";
import { api } from "../api";
import { useSally } from "../store";

const STATUS_KEYS: Record<string, string> = {
  connecting: "statusConnecting",
  live: "statusLive",
  reconnecting: "statusReconnecting",
  paused: "statusPaused",
  ended: "statusEnded",
  idle: "statusIdle",
  "storage-error": "statusStorageError",
  "downloading-models": "statusDownloadingModels",
};

export function TitleBar() {
  const { dict, status, config, setConfig, showSettings, setShowSettings } =
    useSally();
  const [pinned, setPinned] = useState(config?.always_on_top ?? false);

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
        {config?.readout_enabled ? "🔊" : "🔈"}
      </button>
      <button
        className={`icon-btn ${pinned ? "active" : ""}`}
        title={pinned ? dict.unpin : dict.pin}
        onClick={togglePin}
      >
        {pinned ? "📌" : "📍"}
      </button>
      <button
        className={`icon-btn ${showSettings ? "active" : ""}`}
        title={dict.settings}
        onClick={() => setShowSettings(!showSettings)}
      >
        ⚙
      </button>
      <button
        className="icon-btn"
        title="Minimize"
        onClick={() => getCurrentWindow().minimize()}
      >
        —
      </button>
      <button
        className="icon-btn"
        title="Close"
        onClick={() => getCurrentWindow().close()}
      >
        ✕
      </button>
    </div>
  );
}
