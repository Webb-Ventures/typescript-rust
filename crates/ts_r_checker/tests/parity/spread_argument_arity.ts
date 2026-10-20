// A spread of unknown length fits a signature only at or after its required parameters and
// before its last parameter or into a rest one (else TS2556); every spread into a rest
// parameter is checked by position. An IIFE's parameters past its written arguments (a
// spread counts as one) are optional.
export {};
declare const nums: number[];
declare const strs: string[];
declare function two(a: number, b: number): void;
declare function oneRest(a: number, ...r: number[]): void;
declare function rest(...r: (number | symbol)[]): void;
two(...nums);
two(1, ...nums);
oneRest(...nums);
oneRest(1, ...nums);
rest(...nums, ...strs);
rest(...strs, ...nums);
declare const t2: [number, boolean, ...string[]];
(function (a, b, c) {})(...t2);
(function (a, b, c, ...x) {})(...t2);
