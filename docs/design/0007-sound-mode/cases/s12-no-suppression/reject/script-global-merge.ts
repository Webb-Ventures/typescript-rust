// A script file (no imports or exports, on purpose) whose interface merges into the global String.
interface String { // @expect TSR1204
  tsrFixtureShout?(): string;
}
