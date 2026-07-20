// Bottom-right floating controls over the panels: transcript text size
// and window-translucency toggle. Kept out of the title bar so the top
// row stays capture/session-focused.

import { useEffect, useState } from "react";
import { IconContrast } from "./Icons";
import { useSally } from "../store";
import { isTranslucent, setTranslucent } from "../transparency";

const TEXT_SCALE_MIN = 0.85;
const TEXT_SCALE_MAX = 1.4;
const TEXT_SCALE_STEP = 0.1;

export function CornerTools() {
  const { dict } = useSally();
  const [textScale, setTextScale] = useState(() => {
    const saved = Number(localStorage.getItem("sally.textscale"));
    return saved >= TEXT_SCALE_MIN && saved <= TEXT_SCALE_MAX ? saved : 1;
  });
  const [translucent, setTranslucentState] = useState(isTranslucent());

  useEffect(() => {
    document.documentElement.style.setProperty(
      "--text-scale",
      String(textScale)
    );
    localStorage.setItem("sally.textscale", String(textScale));
  }, [textScale]);

  const stepTextScale = (delta: number) => {
    setTextScale((prev) => {
      const next = Math.round((prev + delta) * 100) / 100;
      return Math.min(TEXT_SCALE_MAX, Math.max(TEXT_SCALE_MIN, next));
    });
  };

  const toggleTranslucent = () => {
    const next = !translucent;
    setTranslucentState(next);
    setTranslucent(next);
  };

  return (
    <div className="corner-tools">
      <button
        className="icon-btn text-size-btn text-size-btn-small"
        title={dict.textSizeSmaller}
        onClick={() => stepTextScale(-TEXT_SCALE_STEP)}
      >
        A
      </button>
      <button
        className="icon-btn text-size-btn text-size-btn-big"
        title={dict.textSizeBigger}
        onClick={() => stepTextScale(TEXT_SCALE_STEP)}
      >
        A
      </button>
      <button
        className={`icon-btn ${translucent ? "active" : ""}`}
        title={dict.translucent}
        onClick={toggleTranslucent}
      >
        <IconContrast />
      </button>
    </div>
  );
}
