// Stacked Transcript / Live Translation panels with draggable divider and
// follow-live scrolling (design §6.1–6.2).

import React, { useCallback, useEffect, useRef, useState } from "react";
import { formatTimestamp, TimelineEntry } from "../api";
import { useSally } from "../store";

function useFollowLive(entriesLength: number, partialText: string | undefined) {
  const ref = useRef<HTMLDivElement>(null);
  const [following, setFollowing] = useState(true);

  const onScroll = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const nearBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    setFollowing(nearBottom);
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (el && following) {
      el.scrollTop = el.scrollHeight;
    }
  }, [entriesLength, partialText, following]);

  const jump = useCallback(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
    setFollowing(true);
  }, []);

  return { ref, following, onScroll, jump };
}

const EntryBlock = React.memo(function EntryBlock({
  entry,
  mode,
}: {
  entry: TimelineEntry;
  mode: "original" | "translated";
}) {
  const dict = useSally((s) => s.dict);
  if (entry.kind === "gap") {
    return (
      <div className="entry gap">
        {formatTimestamp(entry.start_ms)}–{formatTimestamp(entry.end_ms)}{" "}
        {dict.gapMarker}
      </div>
    );
  }
  const text = mode === "original" ? entry.original : entry.translated;
  if (!text) return null;
  return (
    <div className="entry">
      <span className="meta">{formatTimestamp(entry.start_ms)}</span>
      <span className="speaker">{entry.speaker}</span>
      <span className="text">{text}</span>
    </div>
  );
});

function Panel({
  title,
  mode,
}: {
  title: string;
  mode: "original" | "translated";
}) {
  const dict = useSally((s) => s.dict);
  const entries = useSally((s) => s.entries);
  const partial = useSally((s) => s.partial);
  const partialText =
    mode === "original" ? partial?.original : partial?.translated;
  const { ref, following, onScroll, jump } = useFollowLive(
    entries.length,
    partialText
  );

  return (
    <div className="panel" style={{ flex: 1, minHeight: 0 }}>
      <div className="panel-header">{title}</div>
      <div className="panel-body" ref={ref} onScroll={onScroll}>
        {entries.length === 0 && !partialText && (
          <div className="empty-hint">{dict.waitingForSpeech}</div>
        )}
        {entries.map((e) => (
          <EntryBlock key={e.index} entry={e} mode={mode} />
        ))}
        {partialText && (
          <div className="entry partial">
            <span className="meta">
              {formatTimestamp(partial?.start_ms ?? 0)}
            </span>
            <span className="text">{partialText}</span>
          </div>
        )}
      </div>
      {!following && (
        <button className="jump-live" onClick={jump}>
          {dict.jumpToLive} ↓
        </button>
      )}
    </div>
  );
}

// Default split: original transcript gets 1/3 of the height, live
// translation gets 2/3 — translation is what most users actually read live.
// Only applies until the user drags the divider once (persisted below).
const DEFAULT_SPLIT_RATIO = 1 / 3;

export function Panels() {
  const dict = useSally((s) => s.dict);
  const [ratio, setRatio] = useState(() => {
    const saved = Number(localStorage.getItem("sally.split"));
    return Number.isFinite(saved) && saved >= 0.15 && saved <= 0.85
      ? saved
      : DEFAULT_SPLIT_RATIO;
  });
  const containerRef = useRef<HTMLDivElement>(null);

  // Pointer capture keeps drags smooth even when the cursor leaves the
  // divider or the window; no global listeners to get stuck.
  const onDividerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onDividerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || rect.height === 0) return;
    setRatio(Math.min(0.85, Math.max(0.15, (e.clientY - rect.top) / rect.height)));
  };
  const onDividerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
      setRatio((r) => {
        localStorage.setItem("sally.split", String(r));
        return r;
      });
    }
  };

  return (
    <div className="panels" ref={containerRef}>
      <div style={{ flex: ratio, display: "flex", minHeight: 60 }}>
        <Panel title={dict.transcript} mode="original" />
      </div>
      <div
        className="divider"
        onPointerDown={onDividerDown}
        onPointerMove={onDividerMove}
        onPointerUp={onDividerUp}
        onPointerCancel={onDividerUp}
      />
      <div style={{ flex: 1 - ratio, display: "flex", minHeight: 60 }}>
        <Panel title={dict.liveTranslation} mode="translated" />
      </div>
    </div>
  );
}
