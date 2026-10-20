// An interface named Window in a module is a local type, not a global augmentation.
interface Window {
  readonly local: true;
}
const w: Window = { local: true };
export const isLocal: boolean = w.local;
