// A let that closures only read keeps its narrowing.
function run(): number {
  let cur: string | null = Math.random() > 0.5 ? "a" : null;
  const peek = (): string | null => cur;
  if (cur !== null) {
    peek();
    return cur.length;
  }
  cur = "b";
  return 0;
}
export const n: number = run();
