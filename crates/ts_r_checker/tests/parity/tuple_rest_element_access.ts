// A literal index past a tuple's leading elements reads the rest element and the elements
// after it; under noUncheckedIndexedAccess `undefined` is added from the count of fixed
// elements on.
export {};
declare const t1: [number, string, ...boolean[]];
declare const t5: [number, ...boolean[], string];
declare const t6: [number, string?, ...boolean[]];
const a: null = t1[2];
const b: null = t1[7];
const c: null = t5[1];
const d: null = t5[3];
const e: null = t6[1];
const f: null = t6[2];
const g: null = t5[0];
