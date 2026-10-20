// A parameter that no closure assigns keeps its narrowing across calls.
function measure(v: string | null, log: () => void): number {
  if (v !== null) {
    log();
    return v.length;
  }
  return 0;
}
export const n: number = measure("a", () => undefined);
