// An interface extending one base without members of its own relates as the base
// (type argument elaboration, both directions).
export {};
interface myArray extends Array<Number> { }
interface myArray3 extends Array<Number> { extra: 1 }
interface Box<T> { v: T; f(): T }
interface NumBox extends Box<number> { }
declare const a: (string | number)[];
declare const b: Box<string>;
declare const nb: NumBox;
declare const ma: myArray;
const c1: myArray = a;
const c2: myArray3 = a;
const c3: NumBox = b;
const c4: Box<string> = nb;
const c5: string[] = ma;
