// Against `X | undefined` a single source (or `boolean`) is reported against `X`;
// a union source keeps the whole target in its head, its members are reported
// against `X`. A spread into an optional parameter meets the parameter type with
// `undefined`.
export {};
declare var sn: string | number;
declare var b: boolean;
declare var sb: string | boolean;
declare var ab: "a" | "b";
const x1: number | undefined = "s";
const x2: number | undefined = sn;
const x3: false | undefined = b;
const x4: number | undefined = sb;
const x5: "a" | undefined = ab;
const x6: number | null = sn;
const x7: { a: number } | undefined = sn;
const x8: string | undefined = sb;
declare function all(a?: number, b?: number): void;
declare var mixed: (number | string)[];
all("x");
all(...mixed);
all(1, ...mixed);
declare function r(a: number, ...b: number[]): void;
r(1, ...mixed);
