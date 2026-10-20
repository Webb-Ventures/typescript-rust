declare const s: symbol;
declare const n: number;
declare const str: string;
const enum E { A = 1, B = 2 }
declare const e: E;
declare const lit: "a" | "b";
if ("a" === "b") {}
if (1 === "x") {}
if (s === true) {}
if (0 === s) {}
if (n === false) {}
if (str === 1) {}
if (e === false) {}
if (lit === "c") {}
if (lit === 1) {}
if (E.A === "x") {}
if ((1 as 1 | 2) === "x") {}
if (str !== 1n) {}
if (lit === true) {}
declare const o: { a: 1 };
if (o === 1) {}
if (1 === undefined!) {}
