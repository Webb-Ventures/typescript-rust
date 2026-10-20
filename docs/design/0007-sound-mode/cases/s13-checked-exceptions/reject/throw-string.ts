// Only Error objects may be thrown.
function fail(reason: string): never {
  throw "failed: " + reason; // @expect TSR1303
}
export const n: number = Math.random() > 2 ? fail("x") : 1;
