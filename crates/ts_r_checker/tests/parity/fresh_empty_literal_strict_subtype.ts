// Under subtype reduction nothing but an empty object is a subtype of a fresh `{}`
// literal: `c ? {} : dict` is the dictionary type.
interface Dictionary<T> {
  [index: string]: T;
}
interface Bar { bar: string; }
declare const d: Dictionary<string>;
declare const c: boolean;
const result = c ? {} : d;
const r2: Record<string, Bar | null> = result;
const result3 = c ? {} : { a: "x" };
const r3: number = result3;
interface DictObj { [index: string]: object }
declare const d2: DictObj;
const result4 = c ? {} : d2;
const r4: number = result4;
