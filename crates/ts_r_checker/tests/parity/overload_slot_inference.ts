// Inference pairs signatures from the end; extra target slots take the first source signature.
type Three<F> = F extends { (...args: infer A1): infer R1; (...args: infer A2): infer R2; (...args: infer A3): infer R3 } ? [A1, A2, A3] : never;
type Two<F> = F extends { (...args: infer A1): infer R1; (...args: infer A2): infer R2 } ? [A1, A2] : never;
declare function g1(c: string): 1;
declare function g1(c: number): 2;
declare function h(c: string): 1;
declare function h(c: number): 2;
declare function h(c: boolean): 3;
export const p1: [Three<typeof g1>, Two<typeof h>] = 1;
