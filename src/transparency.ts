// Window transparency: a CSS variable consumed by the base-app background
// colors in styles.css (sheets/popups stay opaque). The app always starts
// fully opaque; the translucent toggle applies the saved slider level
// (40–100%) for the current run.

const LEVEL_KEY = "sally.alpha";
const MIN = 40;
const MAX = 100;

let translucent = false;
const listeners = new Set<(on: boolean) => void>();

/** Stay in sync with translucency changes made from any component — e.g.
 * the corner-tools toggle button reflecting a change made in Settings. */
export function onTranslucentChange(fn: (on: boolean) => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function loadLevel(): number {
  const raw = Number(localStorage.getItem(LEVEL_KEY));
  if (Number.isFinite(raw) && raw >= MIN && raw <= MAX) return raw;
  return 80;
}

export function isTranslucent(): boolean {
  return translucent;
}

function apply(): void {
  const alpha = translucent ? loadLevel() / 100 : 1;
  document.documentElement.style.setProperty("--app-alpha", String(alpha));
}

export function setTranslucent(on: boolean): void {
  translucent = on;
  apply();
  listeners.forEach((fn) => fn(on));
}

export function setLevel(percent: number): void {
  const clamped = Math.min(MAX, Math.max(MIN, percent));
  localStorage.setItem(LEVEL_KEY, String(clamped));
  apply();
}

/** Startup: opaque regardless of previous session. */
export function initTransparency(): void {
  translucent = false;
  apply();
}
