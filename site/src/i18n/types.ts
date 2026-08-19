export const SITE_LOCALES = ["en", "it", "fr", "es", "de"] as const;
export type Locale = (typeof SITE_LOCALES)[number];

export const DEFAULT_LOCALE: Locale = "en";

export const LOCALE_LABELS: Record<Locale, string> = {
  en: "English",
  it: "Italiano",
  fr: "Français",
  es: "Español",
  de: "Deutsch",
};

export type Feature = { title: string; body: string };

export type Strings = {
  htmlLang: string;
  meta: {
    title: string;
    description: string;
    privacyTitle: string;
    privacyDescription: string;
  };
  hero: {
    gloss: string;
    pitch: string;
    sub: string;
    download: string;
    source: string;
    requires: string;
    iconAlt: string;
  };
  slapTest: {
    ariaReplay: string;
    slapLid: string;
    bumpDesk: string;
    acceleration: string;
    rotation: string;
    fires: string;
    ignored: string;
    footnote: string; // use {pct} placeholder
  };
  discriminator: {
    eyebrow: string;
    heading: string;
    lede: string;
    axisRotation: string;
    caption: string;
  };
  sensitivity: {
    eyebrow: string;
    heading: string;
    lede: string; // {slaps} {quietSeconds}
    label: string;
    caught: string;
    falsePositives: string;
    characters: Record<string, string>; // keyed by "0.20", "0.35", …
  };
  actions: {
    eyebrow: string;
    heading: string;
    lede: string;
    soundTitle: string;
    soundBody: string;
    webhookTitle: string;
    webhookBody: string;
    commandTitle: string;
    commandBody: string;
    advancedMeta: string;
    delayTitle: string;
    delayBody: string;
    severityTitle: string;
    severityBody: string;
  };
  appPanel: {
    eyebrow: string;
    heading: string;
    lede: string;
    followSystem: string;
    bundled: string;
    skipApp: string;
    navMonitor: string;
    navDetection: string;
    navActions: string;
    navAbout: string;
    pause: string;
    acceleration: string;
    rotation: string;
    severityBands: string;
    sensitivityLabel: string;
    cooldownLabel: string;
  };
  privacySection: {
    eyebrow: string;
    heading: string;
    lede: string;
    localTitle: string;
    localBody: string;
    accountTitle: string;
    accountBody: string;
    actionsTitle: string;
    actionsBody: string;
    fullPolicy: string;
    questions: string;
  };
  install: {
    eyebrow: string;
    heading: string;
    worksOn: string;
    worksOnBody: string;
    noSensor: string;
    noSensorBody: string;
    asksFor: string;
    asksForBody: string;
    download: string;
    checksums: string;
    fine: string;
    daemonTitle: string;
    daemonBody: string;
    src: string;
  };
  more: {
    eyebrow: string;
    heading: string;
    lede: string;
    projects: {
      tesserone: Feature;
      makolate: Feature;
      eightySix: Feature;
      chipcolate: Feature;
    };
  };
  footer: {
    claim: string;
    github: string;
    releases: string;
    privacy: string;
    chipcolate: string;
    readme: string;
    legal: string;
    languageLabel: string;
  };
  privacy: {
    back: string;
    title: string;
    effectiveLabel: string;
    effectiveDate: string;
    sections: { title: string; body: string }[];
  };
};
