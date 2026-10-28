// Object literal types already widened by their declarations are not normalized again
// when an array literal joins them (no `dob?: undefined`); fresh literals still are.
var a2 = { name: 'bob', age: 18, address: 'springfield' };
var b2 = { name: 'jim', age: 20, dob: new Date() };
var c2 = [a2, b2];
const n: number = c2;
const fresh = [{ a: 1 }, { b: '' }];
const m: number = fresh;
const mixed = [a2, { b: '' }];
const k: number = mixed;
