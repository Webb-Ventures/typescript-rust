// A destructured parameter whose contextual type is `unknown` (declared, or inferred
// from nothing) takes the binding pattern's own type.
declare function trans<T>(f: (x: T) => string): T;
const r1 = trans(({ a }) => a);
const r2 = trans(([b, c]) => 'foo');
const r3 = trans(({ d: [e, f] }) => 'foo');
declare const p: Promise<unknown>;
p.then(({ data }) => data);
declare function known(f: (x: { a: string }) => string): void;
known(({ a }) => a);
