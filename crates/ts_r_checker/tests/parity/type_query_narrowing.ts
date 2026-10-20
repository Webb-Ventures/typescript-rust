// `typeof x` in a type reads `x` narrowed where it stands.
type U<T> = { kind: "a"; x: T } | { kind: "b"; y: T };
export function f<T>(a: U<T>, b: U<T>) {
  switch (a.kind) {
    case "a":
      return (b as typeof a).x;
  }
}
export function h(a: U<number>, b: U<number>) {
  switch (a.kind) {
    case "a":
      return (b as typeof a).x;
  }
}
export function k<T>(a: U<T>, b: U<T>) {
  if (a.kind === "a") {
    const q: typeof a = b as typeof a;
    return [q.x, a.x];
  }
}
export function m<T>(a: U<T>, b: U<T>) {
  if (a.kind === "a") {
    return (b as typeof a).x;
  }
}
export function g(a: string | number) {
  if (typeof a === "string") {
    let z: typeof a = 1;
    return z;
  }
}
export function u() {
  let x: string;
  type T = typeof x;
  const y: T = "";
  let w: string | undefined;
  w = "s";
  const v: typeof w = undefined;
  return [y, v];
}
