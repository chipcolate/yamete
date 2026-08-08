export const REPO = "https://github.com/chipcolate/yamete";

/** Release landing page — picks up the newest `v*` tag. 404s until one exists. */
export const DOWNLOAD = `${REPO}/releases/latest`;

/** Fixed asset name from release.yml; `/latest/download/` resolves to the current tag. */
export const CHECKSUMS = `${REPO}/releases/latest/download/SHA256SUMS`;

export const SITE = {
  title: "yamete — slap detection for Apple Silicon MacBooks",
  description:
    "Hit the laptop, it makes a noise. A menu bar app that reads the undocumented motion sensor in Apple Silicon MacBooks at 805 Hz and tells a slap from a desk bump.",
};
