// Arrays of object literals are array literal types, each its own recursion identity:
// nesting them is never 'deeply nested', so errors deep inside are found.
interface A { b: B[] }
interface B { c: C }
interface C { d: D[] }
interface D { e: E[] }
interface E { f: F[] }
interface F { g: G }
interface G { h: H[] }
interface H { i: string }
const x: A = { b: [ { c: { d: [ { e: [ { f: [ { g: { h: [ { } ] } } ] } ] } ] } } ] };
const y: D = { e: [ { f: [ { g: { h: [ { } ] } } ] } ] };
const z: C = { d: [ { e: [ { f: [ { g: { h: [ { } ] } } ] } ] } ] };
const w: B = { c: { d: [ { e: [ { f: [ { g: { h: [ { } ] } } ] } ] } ] } };
