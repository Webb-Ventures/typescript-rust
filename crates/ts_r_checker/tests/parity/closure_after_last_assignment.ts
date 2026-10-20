// A closure (function expression or arrow) created after a mutable variable's
// last assignment keeps its narrowing.
declare const m: Map<string, Set<number>>;
declare const p: Promise<void>;
declare function later(f: () => void): void;
function f1(k: string) {
  let set = m.get(k);
  if (!set) {
    set = new Set();
    m.set(k, set);
  }
  set.add(1);
  p.finally(() => set.delete(1));
}
declare function run<T>(cb: (n: number) => Promise<T>): Promise<T>;
declare function launch(n: number): Promise<{ close(): void }>;
async function f2(bs: { close(): void }[]) {
  let browser = bs.find(b => !!b);
  if (!browser) {
    browser = await run(n => launch(n));
  }
  return { dispose: () => browser.close() };
}
function f3(x: string | undefined) {
  if (!x) x = "a";
  const g = () => x.length;
  x = undefined;
  return g;
}
function f4(x: string | undefined) {
  if (!x) x = "a";
  function g() { return x.length; }
  return g;
}
function f5(x: string | undefined) {
  if (!x) x = "a";
  return () => () => x.length;
}
function f6(xs: string[]) {
  let x: string | undefined;
  for (const y of xs) {
    if (x) later(() => x.length);
    x = y;
  }
}
function f7(x: string | undefined) {
  if (!x) x = "a";
  const g = () => { x = undefined; };
  return () => x.length;
}
