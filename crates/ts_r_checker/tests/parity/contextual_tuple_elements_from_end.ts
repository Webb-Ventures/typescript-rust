// Array literal elements after every spread take trailing tuple elements from the end.
export {};
declare function fx1<T extends [string, string, string, 'a' | 'b']>(x: T): T;
declare function fx2<T extends [...string[], 'a' | 'b']>(x: T): T;
const t3 = ['x', 'y', 'z'] as const;
fx1([...t3, 'a']);
fx2([...t3, 'a']);
fx2([...t3, 'c']);
declare const ss: string[];
const q1: [...string[], 'a', 1] = [...ss, 'a', 1];
const q2: [...string[], 'a', 1] = [...ss, 'b', 1];
const q3: [number, ...string[]] = [...ss, 'b'];
declare function fx3<T extends [string, ...('a' | 'b')[]]>(x: T): T;
fx3([...t3, 'a']);
