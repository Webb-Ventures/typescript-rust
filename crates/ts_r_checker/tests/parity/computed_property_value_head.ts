export const SYM = Symbol('a unique symbol');
export interface I { [SYM]: 'sym'; [x: string]: 'str'; }
let a: I = {[SYM]: 'sym'};
let b: I = {[SYM]: 'str'};
let c: { a: 1 } = { ["a"]: 2 };
