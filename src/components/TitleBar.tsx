import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "react";
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
import { useShallow } from "zustand/react/shallow";

const STATUS_KEYS: Record<string, string> = {
  connecting: "statusConnecting",
  live: "statusLive",
  reconnecting: "statusReconnecting",
  paused: "statusPaused",
  ended: "statusEnded",
  idle: "statusIdle",
  "storage-error": "statusStorageError",
};

export function TitleBar() {
  const { dict, status, config, setConfig, showSettings, setShowSettings } =
    useSally(
      useShallow((s) => ({
        dict: s.dict,
        status: s.status,
        config: s.config,
        setConfig: s.setConfig,
        showSettings: s.showSettings,
        setShowSettings: s.setShowSettings,
      }))
    );
  const [pinned, setPinned] = useState(config?.always_on_top ?? false);
  const [volume, setVolume] = useState(config?.readout_volume ?? 1);
  // Remembers the last non-zero volume so clicking the button to unmute
  // (after dragging the slider all the way down) restores something
  // sensible instead of jumping back in at 0.
  const lastVolumeRef = useRef(config?.readout_volume || 1);
  // Mirrors config.readout_enabled synchronously during a drag, so rapid
  // onChange ticks only call setReadout once per mute/unmute boundary
  // crossing instead of on every pixel of movement.
  const enabledRef = useRef(config?.readout_enabled ?? false);

  useEffect(() => {
    setVolume(config?.readout_volume ?? 1);
  }, [config?.readout_volume]);

  useEffect(() => {
    enabledRef.current = config?.readout_enabled ?? false;
  }, [config?.readout_enabled]);

  // Instant while dragging (no .env write per tick), persisted on release.
  // YouTube-style: dragging to 0 mutes, dragging back up unmutes — both
  // fire only on the boundary crossing, not on every tick.
  const dragVolume = (v: number) => {
    setVolume(v);
    api.setReadoutVolume(v, false).catch(() => {});
    if (v > 0) {
      lastVolumeRef.current = v;
      if (!enabledRef.current) {
        enabledRef.current = true;
        api.setReadout(true).then(setConfig).catch(() => {});
      }
    } else if (enabledRef.current) {
      enabledRef.current = false;
      api.setReadout(false).then(setConfig).catch(() => {});
    }
  };
  const commitVolume = async (v: number) => {
    const updated = await api.setReadoutVolume(v, true).catch(() => null);
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
    const next = !(config?.readout_enabled ?? false);
    if (next && volume <= 0) {
      // Unmuting from a slider-dragged-to-zero state: restore the last
      // volume instead of turning on at silence.
      const restored = lastVolumeRef.current || 1;
      setVolume(restored);
      await api.setReadoutVolume(restored, true).catch(() => {});
    }
    enabledRef.current = next;
    const updated = await api.setReadout(next);
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
      <div className="volume-wrap">
        <button
          className={`icon-btn ${config?.readout_enabled ? "active" : ""}`}
          title={config?.readout_enabled ? dict.readoutOff : dict.readoutOn}
          onClick={toggleReadout}
        >
          {config?.readout_enabled ? <IconSpeakerOn /> : <IconSpeakerOff />}
        </button>
        <div className="volume-slider-track">
          <input
            className="volume-slider"
            type="range"
            min={0}
            max={100}
            value={Math.round((config?.readout_enabled ? volume : 0) * 100)}
            title={dict.readoutVolume}
            onChange={(e) => dragVolume(Number(e.target.value) / 100)}
            onPointerUp={() => commitVolume(volume)}
            onKeyUp={() => commitVolume(volume)}
          />
        </div>
      </div>
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
