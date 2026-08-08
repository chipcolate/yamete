// Turns a downsampled trace into SVG path data at build time. No runtime charting
// library, no layout shift, and the paths are identical on every render.

export type Trace = {
  id: string;
  title: string;
  note?: string;
  verdict: "fires" | "ignored";
  fixture: string;
  at: number;
  peak_g: number;
  gyro_peak: number;
  gyro_ratio: number;
  accel: number[];
  gyro: number[];
};

/**
 * Acceleration and rotation get their own stacked panel rather than sharing one plot
 * area. Overlaid on a single frame their heights invite a comparison that means nothing —
 * they are different quantities in different units, so "the rotation line is above the
 * acceleration line" would be an artefact of whichever full-scale values we picked.
 */
export const CHART = { w: 720, accelH: 104, gyroH: 62, pad: 8 } as const;

/**
 * Both events are drawn against the same ceiling, otherwise each trace would be
 * normalised to its own peak and the whole point — that the rejected event is the taller
 * one — would be scaled away. 0.80 g clears desk-bump's 0.7285 g maximum, and 60 °/s
 * clears the slap's 47.7 without leaving the rotation panel looking empty.
 */
export const ACCEL_FULL_SCALE_G = 0.8;
export const GYRO_FULL_SCALE_DPS = 60;

function points(values: number[], fullScale: number, h: number, pad: number) {
  const { w } = CHART;
  const floor = h - pad;
  const span = floor - pad;
  const step = values.length > 1 ? w / (values.length - 1) : w;
  return values.map((v, i) => {
    const y = floor - Math.min(v / fullScale, 1) * span;
    return [i * step, y] as const;
  });
}

/** A polyline through the samples. Impulses are sharp; smoothing would round off the event. */
export function line(values: number[], fullScale: number, h: number, pad = CHART.pad) {
  return points(values, fullScale, h, pad)
    .map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`)
    .join("");
}

/** The same path closed down to the baseline, for the filled area underneath. */
export function area(values: number[], fullScale: number, h: number, pad = CHART.pad) {
  const floor = h - pad;
  return `${line(values, fullScale, h, pad)}L${CHART.w} ${floor}L0 ${floor}Z`;
}
