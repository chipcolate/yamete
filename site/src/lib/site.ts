export const REPO = "https://github.com/chipcolate/yamete";

/** Release landing page — picks up the newest `v*` tag. 404s until one exists. */
export const DOWNLOAD = `${REPO}/releases/latest`;

/** Fixed asset name from release.yml; `/latest/download/` resolves to the current tag. */
export const CHECKSUMS = `${REPO}/releases/latest/download/SHA256SUMS`;

export const SITE = {
  title: "yamete — spank your MacBook",
  description:
    "A free menu bar app for Apple Silicon MacBooks. Spank the lid to play a sound, call a webhook, or run a command. Typing, trackpad clicks, and knocks on the desk stay quiet.",
};
