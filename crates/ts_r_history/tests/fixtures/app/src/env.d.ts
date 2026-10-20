declare const APP_VERSION: string;

declare module "*.svg" {
  const url: string;
  export default url;
}

interface Settings {
  theme: "light" | "dark";
  fontSize: number;
}
