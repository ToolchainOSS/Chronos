/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_MAP_STYLE?: string;
  readonly VITE_BOUNDARIES_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
