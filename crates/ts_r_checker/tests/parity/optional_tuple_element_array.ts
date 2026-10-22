// An optional tuple element adds `undefined` to the tuple's element type and to its array
// base, so iteration and array methods see it; a possibly-undefined optional chain operand
// is named like a property access (`item.id`).
interface Buzz { id: number; buzz: string; }
declare const t: readonly [Buzz?];
const a: number = t.filter(item => true);
declare const t2: [number, string?];
const b: number = t2.map(x => x);
for (const e of t2) { const d: number = e; }
([] as [Buzz] | readonly [Buzz?]).filter(item => item?.id < 5);
