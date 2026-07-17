// Stacked Transcript / Live Translation panels with draggable divider and
// follow-live scrolling (design §6.1–6.2).

import { useCallback, useEffect, useRef, useState } from "react";
import { formatTimestamp, TimelineEntry } from "../api";
import { useSally } from "../store";

function useFollowLive(dep: unknown) {
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
  }, [dep, following]);

  const jump = useCallback(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
    setFollowing(true);
  }, []);

  return { ref, following, onScroll, jump };
}

function EntryBlock({
  entry,
  mode,
}: {
  entry: TimelineEntry;
  mode: "original" | "translated";
}) {
  const { dict } = useSally();
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
}

function Panel({
  title,
  mode,
}: {
  title: string;
  mode: "original" | "translated";
}) {
  const { dict, entries, partial } = useSally();
  const partialText =
    mode === "original" ? partial?.original : partial?.translated;
  const { ref, following, onScroll, jump } = useFollowLive(
    entries.length * 1000 + (partialText?.length ?? 0)
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

export function Panels() {
  const { dict } = useSally();
  const [ratio, setRatio] = useState(() => {
    const saved = localStorage.getItem("sally.split");
    return saved ? Number(saved) : 0.5;
  });
  const containerRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  useEffect(() => {
    const move = (e: MouseEvent) => {
      if (!dragging.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const r = Math.min(0.85, Math.max(0.15, (e.clientY - rect.top) / rect.height));
      setRatio(r);
    };
    const up = () => {
      if (dragging.current) {
        dragging.current = false;
        setRatio((r) => {
          localStorage.setItem("sally.split", String(r));
          return r;
        });
      }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, []);

  return (
    <div className="panels" ref={containerRef}>
      <div style={{ flex: ratio, display: "flex", minHeight: 60 }}>
        <Panel title={dict.transcript} mode="original" />
      </div>
      <div
        className="divider"
        onMouseDown={() => {
          dragging.current = true;
        }}
      />
      <div style={{ flex: 1 - ratio, display: "flex", minHeight: 60 }}>
        <Panel title={dict.liveTranslation} mode="translated" />
      </div>
    </div>
  );
}
