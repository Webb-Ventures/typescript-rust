declare function assertB(c: unknown): asserts c;
declare function assertT(c: unknown): asserts c is string;
const ff = false;
export function g1(): number {
  assertB(false);
}
export function g2(): number {
  assertB(0);
}
export function g3(): number {
  assertB(ff);
}
export function g4(): number {
  assertB(!true);
}
export function g5(): number {
  assertB((false));
}
export function g6(): number {
  assertT(false);
}
export function g7(): number {
  assertB(false && 1);
}
export function g8(): number {
  assertB(null);
}
export function g9(x: string): number {
  assertB(false);
  return x;
}
class Err {
  static invariant(condition: unknown, options: { reason: string }): asserts condition {
    if (!condition) throw new Error(options.reason);
  }
}
declare function assertNever(x: never): never;
declare function assertFn(c: unknown): asserts c;
const ok: typeof assertFn = assertFn;

type K = { kind: "a" } | { kind: "b" } | { kind: "c" };
export function h1(k: K): number {
  switch (k.kind) {
    case "a":
      return 1;
    case "b": {
      Err.invariant(false, { reason: "no" });
    }
    default:
      assertNever(k);
  }
}
export function h2(k: K): number {
  let v: number;
  switch (k.kind) {
    case "a":
      v = 1;
      break;
    case "b":
    case "c":
      assertFn(false);
  }
  return v;
}
export function h3(k: K): number {
  switch (k.kind) {
    case "a":
      return 1;
    default:
      ok(false);
  }
}
