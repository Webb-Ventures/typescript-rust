// Pattern keys read the template literal index signatures they fit.
export {};
declare let combo: { [x: `foo-${string}`]: 'a' | 'b' } & { [x: `${string}-bar`]: 'b' | 'c' };
declare var str: string;
const x4 = combo[`foo-${str}`];
const x5 = combo[`${str}-bar`];
const x6 = combo[`foo-${str}-bar`];
const n4: number = x4;
const n5: number = x5;
const n6: number = x6;
declare let plain: { [x: string]: boolean; [y: `a${string}`]: true };
const p1 = plain[`a${str}`];
const p2 = plain[`b${str}`];
const np1: number = p1;
const np2: number = p2;
declare let ro: { readonly [x: `k${string}`]: number };
ro[`k${str}`] = 1;
