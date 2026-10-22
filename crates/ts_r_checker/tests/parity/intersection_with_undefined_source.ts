interface R { _def: object; go(): void }
export function f<T extends R>(t: T, u: T & undefined, v: T | (T & undefined)) {
  const a: R = u;
  const b: R = v;
  const c: { router: R } = { router: t, ...({} as Partial<{ router: T }>) };
}
