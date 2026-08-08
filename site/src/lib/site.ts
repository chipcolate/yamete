export const REPO = "https://github.com/chipcolate/yamete";

/** Resolves to the newest release's DMG whatever the version is, so the page never
 *  needs editing at release time. 404s until the first `v*` tag is pushed. */
export const DOWNLOAD = `${REPO}/releases/latest`;
export const CHECKSUMS = `${REPO}/releases/latest`;

export const SITE = {
  title: "yamete — slap detection for Apple Silicon MacBooks",
  description:
    "Hit the laptop, it makes a noise. A menu bar app that reads the undocumented motion sensor in Apple Silicon MacBooks at 805 Hz and tells a slap from a desk bump.",
};
