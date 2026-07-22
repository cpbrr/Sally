import { create } from "zustand";
import {
  PartialEntry,
  RedactedConfig,
  ReviewInfo,
  TimelineEntry,
} from "./api";
import { dictionaries, Dict, UiLanguage } from "./i18n";

export type Phase =
  | "boot"
  | "setup"
  | "idle"
  | "live"
  | "saved"
  | "processing";

interface SallyState {
  phase: Phase;
  config: RedactedConfig | null;
  uiLanguage: UiLanguage;
  dict: Dict;
  status: string; // connecting | live | reconnecting | paused | ended | idle | storage-error
  statusDetail: string;
  warning: string;
  entries: TimelineEntry[];
  partial: PartialEntry | null;
  review: ReviewInfo | null;
  pendingRecoveries: number;
  paused: boolean;
  meetingStartedAt: number | null;
  meetingEndedAt: number | null;
  pausedAccumMs: number;
  pausedSince: number | null;
  showSettings: boolean;
  micLost: boolean;

  setPhase: (p: Phase) => void;
  setConfig: (c: RedactedConfig | null) => void;
  setUiLanguage: (l: UiLanguage) => void;
  setStatus: (state: string, detail: string) => void;
  setWarning: (w: string) => void;
  addEntry: (e: TimelineEntry) => void;
  setPartial: (p: PartialEntry | null) => void;
  setReview: (r: ReviewInfo | null) => void;
  setPendingRecoveries: (n: number) => void;
  startMeetingClock: () => void;
  stopMeetingClock: () => void;
  setPaused: (paused: boolean) => void;
  resetMeeting: () => void;
  setShowSettings: (v: boolean) => void;
  setMicLost: (v: boolean) => void;
}

export const useSally = create<SallyState>((set, get) => ({
  phase: "boot",
  config: null,
  uiLanguage: "vi",
  dict: dictionaries.vi,
  status: "idle",
  statusDetail: "",
  warning: "",
  entries: [],
  partial: null,
  review: null,
  pendingRecoveries: 0,
  paused: false,
  meetingStartedAt: null,
  meetingEndedAt: null,
  pausedAccumMs: 0,
  pausedSince: null,
  showSettings: false,
  micLost: false,

  setPhase: (phase) => set({ phase }),
  setConfig: (config) => {
    const lang = (
      config?.ui_language === "en" ||
      config?.ui_language === "vi" ||
      config?.ui_language === "ja"
        ? config.ui_language
        : "vi"
    ) as UiLanguage;
    set({ config, uiLanguage: lang, dict: dictionaries[lang] });
  },
  setUiLanguage: (uiLanguage) =>
    set({ uiLanguage, dict: dictionaries[uiLanguage] }),
  setStatus: (status, statusDetail) => set({ status, statusDetail }),
  setWarning: (warning) => set({ warning }),
  addEntry: (e) => set({ entries: [...get().entries, e] }),
  setPartial: (partial) => set({ partial }),
  setReview: (review) => set({ review }),
  setPendingRecoveries: (pendingRecoveries) => set({ pendingRecoveries }),
  startMeetingClock: () =>
    set({
      meetingStartedAt: Date.now(),
      meetingEndedAt: null,
      pausedAccumMs: 0,
      pausedSince: null,
      paused: false,
    }),
  stopMeetingClock: () => set({ meetingEndedAt: Date.now() }),
  setPaused: (paused) => {
    const s = get();
    if (paused && !s.paused) {
      set({ paused, pausedSince: Date.now() });
    } else if (!paused && s.paused) {
      const extra = s.pausedSince ? Date.now() - s.pausedSince : 0;
      set({ paused, pausedAccumMs: s.pausedAccumMs + extra, pausedSince: null });
    }
  },
  resetMeeting: () =>
    set({
      entries: [],
      partial: null,
      review: null,
      paused: false,
      meetingStartedAt: null,
      meetingEndedAt: null,
      pausedAccumMs: 0,
      pausedSince: null,
      status: "idle",
      statusDetail: "",
      warning: "",
      micLost: false,
    }),
  setShowSettings: (showSettings) => set({ showSettings }),
  setMicLost: (micLost) => set({ micLost }),
}));
