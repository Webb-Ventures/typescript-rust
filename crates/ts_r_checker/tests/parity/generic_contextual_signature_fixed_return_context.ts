// Inside a function contextually typed by a generic signature, that signature's type
// parameters are fixed types: a returned generic call infers from the return context `R`
// (an uninferred `R` of the callee becomes the outer `R`, not its default).
declare function run<T extends Array<any>, R = T[]>(o: {
  q: readonly [...T];
  combine?: (r: T) => R;
}): R;
type UseRun = <T extends any[], R = T[]>(
  cb: (x: number) => readonly [...T],
  options?: { combine?: (r: T) => R },
) => R;
export const f: UseRun = (cb, options) => {
  const qs = cb(1);
  return run({ q: qs, combine: options?.combine as any });
};
export const g: UseRun = function (cb) {
  return run({ q: cb(2) });
};
export const h: UseRun = (cb) => {
  const n: number = run({ q: cb(3) });
  return run({ q: cb(4), combine: 1 as any });
};
export const k: UseRun = (cb) => [cb(5)];
