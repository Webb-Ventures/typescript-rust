// Global augmentation changes types for the whole program.
declare global { // @expect TSR1204
  interface Window {
    tsrFixtureState?: string;
  }
}
export {};
