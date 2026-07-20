// Cheap platform check for UI-only branching (which capture-method options
// to show, whether the Bluetooth-mic hint applies). Not used for anything
// security- or correctness-sensitive — the Rust side is the source of truth
// for what's actually supported on each OS.
export function isMac(): boolean {
  return navigator.platform.toLowerCase().includes("mac")
    || navigator.userAgent.toLowerCase().includes("mac os");
}
