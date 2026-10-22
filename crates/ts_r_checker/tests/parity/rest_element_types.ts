type F1 = (...a: string) => void;
interface I { m(...a: number): void; (...b: {}): void; new (...c: boolean): I; }
type C1 = new (...a: string) => void;
type Ok<T extends any[]> = (...a: T) => void;
type Inf<F> = F extends (...a: infer A) => void ? (...b: A) => void : never;
type R1 = [...string];
type R2 = [...string[], ...number[]];
type R3 = [...string[], boolean?];
type A = any[];
type R4 = [...A, ...A];
type R5 = [...[number, ...string[]], ...boolean[]];
type R6 = [...[number], ...boolean[]];
type R7<T extends unknown[]> = [...T, ...T];
type R8<T> = T extends [infer H, ...infer Rest] ? [...Rest, H] : never;
type R9 = [...(string[]), ...[...number[]]];
type R10 = [...null];
export {};
