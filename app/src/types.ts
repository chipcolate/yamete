//! Mirrors of the daemon's wire types.
//!
//! Kept hand-written rather than generated: the protocol is small, and a generator would
//! be more machinery than the thing it generates.

export type Tier = "micro" | "medium" | "major";

export type GyroMode = "off" | "annotate" | "require";

export interface Slap {
  t: number;
  tier: Tier;
  peak_g: number;
  intensity: number;
  votes: number;
  gyro_confirmed: boolean;
  gyro_peak: number;
  gyro_ratio: number;
  axis: [number, number, number];
}

export interface Scores {
  envelope: number;
  sta_lta: number;
  cusum: number;
  kurtosis: number;
  peak_mad: number;
  gyro: number;
  gyro_peak: number;
  votes: number;
  in_cooldown: boolean;
}

export interface Telemetry {
  t: number;
  envelope: number[];
  gyro: number[];
  scores: Scores;
  dropped: number;
}

export interface Status {
  enabled: boolean;
  version: string;
  has_gyro: boolean;
  uptime_s: number;
  slaps: number;
  rate_hz: number;
  warming_up: boolean;
  telemetry_subscribers: number;
}

export interface Tiers {
  major_g: number;
  medium_g: number;
  micro_g: number;
}

export interface Thresholds {
  sta_lta: number;
  cusum: number;
  kurtosis: number;
  peak_mad: number;
  highpass_g: number;
}

export interface DetectorConfig {
  rate_hz: number;
  highpass_hz: number;
  sta_taus: [number, number, number];
  lta_tau: number;
  thresholds: Thresholds;
  tiers: Tiers;
  cooldown_s: number;
  peak_hold_s: number;
  cusum_leak: number;
  gyro_mode: GyroMode;
  gyro_ratio_min: number;
  gyro_peak_min: number;
  gyro_window_s: number;
  min_votes: number;
  full_scale_g: number;
  sensitivity: number;
}

export type ActionKind =
  | {
      type: "sound";
      paths: string[];
      order: "sequential" | "random";
      volume_db: number;
      scale_with_intensity: boolean;
      intensity_range_pct: number;
      playback_rate: number;
    }
  | { type: "exec"; program: string; args: string[]; stdin_json: boolean }
  | {
      type: "webhook";
      url: string;
      method: string;
      headers: Record<string, string>;
      body: string | null;
      timeout_ms: number;
    };

export interface Action {
  id: string;
  name: string;
  enabled: boolean;
  tiers: Tier[];
  min_intensity: number;
  delay_ms: number;
  kind: ActionKind;
}

export interface DaemonConfig {
  enabled: boolean;
  detector: DetectorConfig;
  actions: Action[];
  report_interval_us: number;
}
