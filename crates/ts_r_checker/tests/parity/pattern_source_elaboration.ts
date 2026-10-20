// Template literal and string mapping sources elaborate through their constraint:
// mappings through string (or their own kind's argument), templates through widened holes.
export {};
declare let a1: `foo${string}`;
declare let a2: Uppercase<string>;
declare let a3: `AA${Uppercase<string>}`;
declare let a4: Capitalize<string>;
declare let a5: `${number}`;
let b1: number = a1;
let b2: number = a2;
let b3: "AA" = a3;
let b4: `A${string}` = a4;
let b5: "x" = a1;
let b6: "x" = a2;
let b7: Uppercase<Lowercase<string>> = a2;
let b8: Lowercase<string> = a2;
let b9: `bar${string}` = a1;
let b10: `bar${string}` = a3;
let b11: boolean = a5;
let b12: "1" = a5;
let b13: { x: number } = a1;
let b14: Lowercase<string> = a1;
