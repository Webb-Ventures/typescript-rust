// Redeclared variables must have identical types: generic signatures need as many type
// parameters with identical constraints; written `any` is compared exactly.
var f: <T, U>(x: T, y: U) => T;
var f: <T, U>(x: any, y: any) => any;
var g: <T, U>(x: T, y: U) => T;
var g: <T>(x: any, y: any) => any;
var h: <T, U>(x: T, y: U) => T;
var h: (x: any, y: any) => any;
var x: <T extends Date>(x: T) => T;
var x: <T extends number>(x: T) => T;
var x: <T>(x: T) => T;
var w: { w: string };
var w: { w: number };
var v: (v: number) => number;
var v: (v: number) => number;
var m: (n: any) => any;
var m = function (n) { return n; };
