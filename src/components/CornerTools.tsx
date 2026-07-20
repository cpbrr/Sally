// Bottom-right floating controls over the panels: transcript text size
// and window-translucency toggle. Kept out of the title bar so the top
// row stays capture/session-focused.

import { useEffect, useState } from "react";
import { IconContrast } from "./Icons";
import { useSally } from "../store";
import { isTranslucent, setTranslucent } from "../transparency";

const TEXT_SCALES = [1, 1.15, 1.3, 0.9];

export function CornerTools() {
  const { dict } = useSally();
  const [textScale, setTextScale] = useState(() => {
    const saved = Number(localStorage.getItem("sally.textscale"));
    return TEXT_SCALES.includes(saved) ? saved : 1;
  });
  const [translucent, setTranslucentState] = useState(isTranslucent());

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

  const toggleTranslucent = () => {
    const next = !translucent;
    setTranslucentState(next);
    setTranslucent(next);
  };

  return (
    <div className="corner-tools">
      <button
        className="icon-btn text-size-btn"
        title={dict.textSize}
        onClick={cycleTextScale}
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
