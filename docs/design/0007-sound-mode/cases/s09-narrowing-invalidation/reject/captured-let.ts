// A let assigned inside a closure can change when the closure runs.
function run(): number {
  let cur: string | null = Math.random() > 0.5 ? "a" : null;
  const reset = (): void => {
    cur = null;
  };
  if (cur !== null) {
    reset();
    return cur.length; // @expect TSR0902
  }
  return 0;
}
export const n: number = run();
