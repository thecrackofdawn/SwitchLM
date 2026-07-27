// Naive UI GlobalThemeOverrides mapped from the instrument-panel tokens (src/styles/tokens.css).
// Light overrides are applied with the default theme; dark overrides with naive-ui's darkTheme.
import type { GlobalThemeOverrides } from "naive-ui";

const fontSans = '"IBM Plex Sans", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif';
const fontMono = '"IBM Plex Mono", "Cascadia Mono", Consolas, monospace';

export const lightOverrides: GlobalThemeOverrides = {
  common: {
    primaryColor: "#3B5BDB",
    primaryColorHover: "#3355C9",
    primaryColorPressed: "#2A48B0",
    primaryColorSuppl: "#3B5BDB",
    bodyColor: "#F5F6F8",
    cardColor: "#FFFFFF",
    modalColor: "#FFFFFF",
    popoverColor: "#FFFFFF",
    textColorBase: "#1B1F24",
    textColor1: "#1B1F24",
    textColor2: "#1B1F24",
    textColor3: "#5C6370",
    textColorDisabled: "#8B949E",
    placeholderColor: "#8B949E",
    dividerColor: "#E1E4E8",
    borderColor: "#E1E4E8",
    borderRadius: "8px",
    borderRadiusSmall: "5px",
    fontFamily: fontSans,
    fontFamilyMono: fontMono,
    fontWeightStrong: "600",
  },
  Card: {
    color: "#FFFFFF",
    borderColor: "#E1E4E8",
  },
};

export const darkOverrides: GlobalThemeOverrides = {
  common: {
    primaryColor: "#5B7CFA",
    primaryColorHover: "#6E8CFB",
    primaryColorPressed: "#4A6AE0",
    primaryColorSuppl: "#5B7CFA",
    bodyColor: "#0E1116",
    cardColor: "#161B22",
    modalColor: "#161B22",
    popoverColor: "#1C232C",
    textColorBase: "#E6EDF3",
    textColor1: "#E6EDF3",
    textColor2: "#E6EDF3",
    textColor3: "#9AA6B2",
    textColorDisabled: "#6B7785",
    placeholderColor: "#6B7785",
    dividerColor: "#30363D",
    borderColor: "#30363D",
    borderRadius: "8px",
    borderRadiusSmall: "5px",
    fontFamily: fontSans,
    fontFamilyMono: fontMono,
    fontWeightStrong: "600",
  },
  Card: {
    color: "#161B22",
    borderColor: "#30363D",
  },
};
