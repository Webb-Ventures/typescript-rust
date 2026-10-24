declare const s: unique symbol;
declare const r: unique symbol;
type A = { [s]?: 1; [r]: 1 };
type M = { [P in keyof A]: number };

function all<K extends keyof A>(m: M, k: K): number {
  return m[k];
}
function optional<K extends typeof s>(m: M, k: K): number {
  return m[k];
}
function required<K extends typeof r>(m: M, k: K): number {
  return m[k];
}
function nullable<K extends keyof A>(m: M, k: K): number | undefined {
  return m[k];
}

type RequiredM = { [P in keyof A]-?: number };
function removed<K extends keyof A>(m: RequiredM, k: K): number {
  return m[k];
}
