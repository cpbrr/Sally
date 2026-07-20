// Typed boundary to the Rust core. The UI renders state received from Rust
// and never captures audio, calls Gemini, or writes meeting files itself.

import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export interface RedactedConfig {
  data_dir: string;
  has_api_key: boolean;
  live_model: string;
  cleanup_model: string;
  target_language: string;
  ui_language: string;
  always_on_top: boolean;
  mic_device: string;
  system_device: string;
  capture_app: string;
  readout_enabled: boolean;
  save_audio: boolean;
  readout_speed: number;
}

export interface BootInfo {
  config: RedactedConfig | null;
  needs_setup: boolean;
  pending_recoveries: number;
}

export interface SettingsPayload {
  data_dir?: string;
  api_key?: string;
  live_model?: string;
  cleanup_model?: string;
  target_language?: string;
  ui_language?: string;
  always_on_top?: boolean;
  mic_device?: string;
  system_device?: string;
  capture_app?: string;
  readout_enabled?: boolean;
  save_audio?: boolean;
  readout_speed?: number;
}

export interface AudioDevices {
  inputs: string[];
  outputs: string[];
}

export interface TimelineEntry {
  index: number;
  kind: "speech" | "gap";
  start_ms: number;
  end_ms: number;
  speaker: string;
  original: string;
  translated: string;
}

export interface PartialEntry {
  start_ms: number;
  speaker: string;
  original: string;
  translated: string;
}

export interface StatusPayload {
  state: string;
  detail: string;
}

export interface MeetingFile {
  name: string;
  path: string;
}

export interface ReviewInfo {
  raw_path: string;
  raw_dir: string;
  polished_dir: string;
  speakers: string[];
  audio_path: string | null;
}

export interface TranscriptChunk {
  start_ms: number;
  speaker: string;
  text: string;
}

export const api = {
  getBootInfo: () => invoke<BootInfo>("get_boot_info"),
  getApiKey: () => invoke<string>("get_api_key"),
  saveSettings: (payload: SettingsPayload) =>
    invoke<RedactedConfig>("save_settings", { payload }),
  listAudioDevices: () => invoke<AudioDevices>("list_audio_devices"),
  listAudioApps: () => invoke<string[]>("list_audio_apps"),
  testConnectivity: () => invoke<boolean>("test_connectivity"),
  startMeeting: (targetLanguage?: string) =>
    invoke<void>("start_meeting", { targetLanguage: targetLanguage ?? null }),
  pauseMeeting: () => invoke<void>("pause_meeting"),
  setReadout: (enabled: boolean) =>
    invoke<RedactedConfig>("set_readout", { enabled }),
  resumeMeeting: () => invoke<void>("resume_meeting"),
  endMeeting: () => invoke<ReviewInfo>("end_meeting"),
  getLastMeeting: () => invoke<ReviewInfo | null>("get_last_meeting"),
  listMeetings: () => invoke<MeetingFile[]>("list_meetings"),
  openMeeting: (rawPath: string) =>
    invoke<ReviewInfo>("open_meeting", { rawPath }),
  meetingChunks: () => invoke<TranscriptChunk[]>("meeting_chunks"),
  applyReview: (renames: Record<string, string>, meetingTitle?: string) =>
    invoke<ReviewInfo>("apply_review", {
      renames,
      meetingTitle: meetingTitle ?? null,
    }),
  exportWithoutTimestamps: () => invoke<string>("export_without_timestamps"),
  cleanAndSummarize: (includeTimestamps: boolean) =>
    invoke<string>("clean_and_summarize", { includeTimestamps }),
  recoverMeetings: () => invoke<string[]>("recover_meetings"),
};

export function onEntry(cb: (e: TimelineEntry) => void): Promise<UnlistenFn> {
  return listen<TimelineEntry>("sally://entry", (ev) => cb(ev.payload));
}

export function onPartial(
  cb: (e: PartialEntry | null) => void
): Promise<UnlistenFn> {
  return listen<PartialEntry | null>("sally://partial", (ev) =>
    cb(ev.payload)
  );
}

export function onStatus(cb: (s: StatusPayload) => void): Promise<UnlistenFn> {
  return listen<StatusPayload>("sally://status", (ev) => cb(ev.payload));
}

export function onWarning(cb: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>("sally://warning", (ev) => cb(ev.payload));
}

export interface DiarizePayload {
  state: "running" | "done" | "failed";
  detail: string;
}

export function onDiarize(cb: (p: DiarizePayload) => void): Promise<UnlistenFn> {
  return listen<DiarizePayload>("sally://diarize", (ev) => cb(ev.payload));
}

export function formatTimestamp(ms: number): string {
  const totalS = Math.floor(ms / 1000);
  const h = Math.floor(totalS / 3600);
  const m = Math.floor((totalS % 3600) / 60);
  const s = totalS % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}
