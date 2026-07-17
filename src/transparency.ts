// Window transparency: a CSS variable consumed by the background colors in
// styles.css. Persisted locally per machine (window chrome preference, not
// meeting data), restored at startup by App.

const KEY = "sally.alpha";
const MIN = 10;
const MAX = 100;

export function loadTransparency(): number {
  const raw = Number(localStorage.getItem(KEY));
  if (Number.isFinite(raw) && raw >= MIN && raw <= MAX) return raw;
  return MAX;
}

export function applyTransparency(percent: number): void {
  const clamped = Math.min(MAX, Math.max(MIN, percent));
  document.documentElement.style.setProperty(
    "--app-alpha",
    String(clamped / 100)
  );
  localStorage.setItem(KEY, String(clamped));
}
