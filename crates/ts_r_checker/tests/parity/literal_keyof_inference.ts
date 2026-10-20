// String literal keys inferred to `keyof T` give `T` an object with those keys typed `any`
// (intersected across arguments, used only without other inferences). `o?.m()` infers a
// `this` parameter from `o` without `undefined`.
export {};
declare function inference1<T>(name: keyof T): T;
declare var two: "a" | "d";
const x = inference1(two);
const xs: string = x;
declare function bar<T>(x: keyof T, y: keyof T): T;
const c = bar("a", "b");
const cs: string = c;
declare function inference2<T>(target: T, name: keyof T): T;
const y = inference2({ a: 1, b: 2 }, "a");
const ys: string = y;
interface Y {
  foo<T>(this: T, arg: keyof T): void;
  a: number;
}
declare const value: Y | undefined;
value?.foo("a");
value?.foo("q");
