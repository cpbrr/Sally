// Vector icon set replacing the emoji buttons. Line style, currentColor —
// buttons inherit the app palette (dim text, accent when active) matching
// the microphone logo.

import { SVGProps } from "react";

function Base(props: SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={15}
      height={15}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...props}
    />
  );
}

export const IconSpeakerOn = () => (
  <Base>
    <path d="M11 5 6 9H2v6h4l5 4V5z" />
    <path d="M15.5 8.5a5 5 0 0 1 0 7" />
    <path d="M18.8 5.8a9.5 9.5 0 0 1 0 12.4" />
  </Base>
);

export const IconSpeakerOff = () => (
  <Base>
    <path d="M11 5 6 9H2v6h4l5 4V5z" />
    <line x1={16} y1={9.5} x2={21} y2={14.5} />
    <line x1={21} y1={9.5} x2={16} y2={14.5} />
  </Base>
);

export const IconContrast = () => (
  <Base>
    <circle cx={12} cy={12} r={9} />
    <path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" stroke="none" />
  </Base>
);

export const IconSwap = () => (
  <Base>
    <path d="M4 8h13l-3.5-3.5" />
    <path d="M20 16H7l3.5 3.5" />
  </Base>
);

export const IconPin = () => (
  <Base>
    <path d="M9 3h6l-1 7 3.5 3.5H6.5L10 10 9 3z" />
    <line x1={12} y1={13.5} x2={12} y2={21} />
  </Base>
);

export const IconPinOff = () => (
  <Base>
    <path d="M9 3h6l-1 7 3.5 3.5H6.5L10 10 9 3z" />
    <line x1={12} y1={13.5} x2={12} y2={21} />
    <line x1={4} y1={4} x2={20} y2={20} />
  </Base>
);

export const IconGear = () => (
  <Base>
    <circle cx={12} cy={12} r={7.3} />
    <circle cx={12} cy={12} r={2.4} />
    <line x1={19.3} y1={12} x2={21.6} y2={12} strokeWidth={3} />
    <line x1={17.16} y1={17.16} x2={18.79} y2={18.79} strokeWidth={3} />
    <line x1={12} y1={19.3} x2={12} y2={21.6} strokeWidth={3} />
    <line x1={6.84} y1={17.16} x2={5.21} y2={18.79} strokeWidth={3} />
    <line x1={4.7} y1={12} x2={2.4} y2={12} strokeWidth={3} />
    <line x1={6.84} y1={6.84} x2={5.21} y2={5.21} strokeWidth={3} />
    <line x1={12} y1={4.7} x2={12} y2={2.4} strokeWidth={3} />
    <line x1={17.16} y1={6.84} x2={18.79} y2={5.21} strokeWidth={3} />
  </Base>
);

export const IconMinus = () => (
  <Base>
    <line x1={5} y1={12} x2={19} y2={12} />
  </Base>
);

export const IconClose = () => (
  <Base>
    <line x1={6} y1={6} x2={18} y2={18} />
    <line x1={18} y1={6} x2={6} y2={18} />
  </Base>
);

export const IconDoc = () => (
  <Base>
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6z" />
    <path d="M14 2v6h6" />
    <line x1={8} y1={13} x2={16} y2={13} />
    <line x1={8} y1={17} x2={13} y2={17} />
  </Base>
);

export const IconFolder = () => (
  <Base>
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2v11z" />
  </Base>
);

export const IconEye = () => (
  <Base>
    <path d="M1.5 12S5.5 5 12 5s10.5 7 10.5 7-4 7-10.5 7S1.5 12 1.5 12z" />
    <circle cx={12} cy={12} r={3} />
  </Base>
);

export const IconEyeOff = () => (
  <Base>
    <path d="M1.5 12S5.5 5 12 5s10.5 7 10.5 7-4 7-10.5 7S1.5 12 1.5 12z" />
    <circle cx={12} cy={12} r={3} />
    <line x1={4} y1={4} x2={20} y2={20} />
  </Base>
);

export const IconWarning = () => (
  <Base>
    <path d="M12 3 22 20H2z" />
    <line x1={12} y1={9.5} x2={12} y2={14.5} />
    <circle cx={12} cy={17.3} r={0.6} fill="currentColor" stroke="none" />
  </Base>
);

export const IconRefresh = () => (
  <Base>
    <path d="M21 4v6h-6" />
    <path d="M3 20v-6h6" />
    <path d="M5 10a7.5 7.5 0 0 1 12.7-3.4L21 10" />
    <path d="M19 14a7.5 7.5 0 0 1-12.7 3.4L3 14" />
  </Base>
);

export const IconReset = () => (
  <Base>
    <path d="M3 5v6h6" />
    <path d="M4.5 11A8 8 0 1 1 7 17.7" />
  </Base>
);

export const IconMic = () => (
  <Base>
    <rect x={9} y={2} width={6} height={12} rx={3} />
    <path d="M18 10v1a6 6 0 0 1-12 0v-1" />
    <line x1={12} y1={19} x2={12} y2={22} />
    <line x1={8} y1={22} x2={16} y2={22} />
  </Base>
);

export const IconMicOff = () => (
  <Base>
    <rect x={9} y={2} width={6} height={12} rx={3} />
    <path d="M18 10v1a6 6 0 0 1-12 0v-1" />
    <line x1={12} y1={19} x2={12} y2={22} />
    <line x1={8} y1={22} x2={16} y2={22} />
    <line x1={4} y1={4} x2={20} y2={20} />
  </Base>
);

export const IconChevron = ({ open }: { open: boolean }) => (
  <Base style={{ transform: open ? "rotate(90deg)" : undefined, transition: "transform 0.12s" }}>
    <polyline points="9 6 15 12 9 18" />
  </Base>
);
