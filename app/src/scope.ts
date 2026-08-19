//! The live scope.
//!
//! A rolling view of the high-passed acceleration envelope with the tier bands drawn over
//! it, so choosing a sensitivity is a matter of looking at where your spanks land relative
//! to the lines rather than guessing at numbers.

import type { Telemetry, Tiers } from "./types";

/** Seconds of history on screen. */
const WINDOW_S = 6;

/** Assumed sample rate for sizing the ring; the exact value only affects history length. */
const NOMINAL_RATE = 805;

/** Vertical range, in g. Chosen to cover a hard spank with headroom. */
const MAX_G = 0.8;

interface Marker {
  /**
   * Total samples written when the spank landed.
   *
   * Deliberately not a ring index: once the ring wraps, an old index maps onto a
   * perfectly valid position holding newer data, so markers from minutes ago reappear as
   * ghosts against unrelated signal. A monotonic count cannot alias.
   */
  at: number;
  tier: string;
}

export class Scope {
  private ctx: CanvasRenderingContext2D;
  private envelope: Float32Array;
  private gyro: Float32Array;
  private write = 0;
  private filled = 0;
  /** Total samples ever pushed, used to place markers without ring aliasing. */
  private written = 0;
  private markers: Marker[] = [];
  private tiers: Tiers | null = null;
  private dpr = 1;

  constructor(private canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("could not get a 2d context");
    this.ctx = ctx;

    const capacity = WINDOW_S * NOMINAL_RATE;
    this.envelope = new Float32Array(capacity);
    this.gyro = new Float32Array(capacity);
    this.resize();
  }

  resize() {
    this.dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    this.canvas.width = Math.max(1, Math.round(rect.width * this.dpr));
    this.canvas.height = Math.max(1, Math.round(rect.height * this.dpr));
  }

  setTiers(tiers: Tiers) {
    this.tiers = tiers;
  }

  push(frame: Telemetry) {
    const n = this.envelope.length;
    // Gyro frames arrive interleaved with accel ones and in similar numbers; pairing them
    // by index is close enough for a visual, and avoids carrying timestamps per sample.
    for (let i = 0; i < frame.envelope.length; i++) {
      this.envelope[this.write] = frame.envelope[i] ?? 0;
      this.gyro[this.write] = frame.gyro[i] ?? this.gyro[(this.write - 1 + n) % n] ?? 0;
      this.write = (this.write + 1) % n;
      this.written++;
      if (this.filled < n) this.filled++;
    }
  }

  markSpank(tier: string) {
    this.markers.push({ at: this.written, tier });
    // Anything older than the window can never be drawn again.
    const oldest = this.written - this.envelope.length;
    this.markers = this.markers.filter((m) => m.at >= oldest);
  }

  /** Map a value in g to a y coordinate. */
  private y(value: number, height: number): number {
    const clamped = Math.min(Math.max(value, 0), MAX_G);
    return height - (clamped / MAX_G) * height;
  }

  draw() {
    const { ctx } = this;
    const w = this.canvas.width;
    const h = this.canvas.height;
    const style = getComputedStyle(document.documentElement);
    const bg = style.getPropertyValue("--scope-bg").trim() || "#0d0f14";
    const line = style.getPropertyValue("--scope-line").trim() || "#4ade80";
    const grid = style.getPropertyValue("--scope-grid").trim() || "#1e2430";
    const gyroColor = style.getPropertyValue("--scope-gyro").trim() || "#3b82f6";

    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, w, h);

    // Tier bands: the whole point of the scope is seeing where a hit lands against these.
    if (this.tiers) {
      const bands: Array<[number, string, string]> = [
        [this.tiers.major_g, "major", "rgba(239,68,68,0.55)"],
        [this.tiers.medium_g, "medium", "rgba(245,158,11,0.55)"],
        [this.tiers.micro_g, "micro", "rgba(148,163,184,0.45)"],
      ];
      ctx.setLineDash([4 * this.dpr, 4 * this.dpr]);
      ctx.lineWidth = this.dpr;
      ctx.font = `${11 * this.dpr}px ui-monospace, monospace`;
      ctx.textBaseline = "bottom";
      for (const [value, label, color] of bands) {
        const y = this.y(value, h);
        if (y < 0 || y > h) continue;
        ctx.strokeStyle = color;
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
        ctx.stroke();
        ctx.fillStyle = color;
        ctx.fillText(label, 4 * this.dpr, y - 2 * this.dpr);
      }
      ctx.setLineDash([]);
    } else {
      ctx.strokeStyle = grid;
      ctx.lineWidth = this.dpr;
      for (let i = 1; i < 4; i++) {
        const y = (h * i) / 4;
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
        ctx.stroke();
      }
    }

    if (this.filled === 0) return;

    const n = this.envelope.length;
    const start = (this.write - this.filled + n) % n;
    // More samples than pixels, so take the max per column: a one-sample spike is exactly
    // what matters here and averaging would hide it.
    const perPixel = Math.max(1, Math.floor(this.filled / w));

    const trace = (data: Float32Array, color: string, scale: number, width: number) => {
      ctx.strokeStyle = color;
      ctx.lineWidth = width * this.dpr;
      ctx.beginPath();
      let x = 0;
      for (let i = 0; i < this.filled; i += perPixel) {
        let peak = 0;
        for (let k = 0; k < perPixel && i + k < this.filled; k++) {
          const v = data[(start + i + k) % n] ?? 0;
          if (v > peak) peak = v;
        }
        const px = (x / Math.ceil(this.filled / perPixel)) * w;
        const py = this.y(peak * scale, h);
        if (x === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
        x++;
      }
      ctx.stroke();
    };

    // Angular rate shares the axis, scaled so a corroborating rotation is visible next to
    // the acceleration that produced it. Not a physical comparison, just a legible one.
    trace(this.gyro, gyroColor, MAX_G / 120, 1);
    trace(this.envelope, line, 1, 1.5);

    // Spank markers, placed by age rather than by ring position.
    const oldest = this.written - this.filled;
    ctx.textBaseline = "top";
    for (const marker of this.markers) {
      const offset = marker.at - oldest;
      if (offset < 0 || offset > this.filled) continue;
      const px = (offset / this.filled) * w;
      ctx.strokeStyle =
        marker.tier === "major"
          ? "rgba(239,68,68,0.9)"
          : marker.tier === "medium"
            ? "rgba(245,158,11,0.9)"
            : "rgba(148,163,184,0.8)";
      ctx.lineWidth = this.dpr;
      ctx.beginPath();
      ctx.moveTo(px, 0);
      ctx.lineTo(px, h);
      ctx.stroke();
    }
  }
}
