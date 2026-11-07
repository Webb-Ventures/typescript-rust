// A context-sensitive callback typed by an earlier overload pass fixes no type parameter
// of a later generic candidate: its return still infers (`U = any` here), where a single
// generic candidate fixes `U` from `init` first (TypeORM's `incrementOrDecrementBy`).
declare function ov(cb: (prev: string, cur: string) => string, init: string): string;
declare function ov<U>(cb: (prev: U, cur: string) => U, init: U): U;
declare function one<U>(cb: (prev: U, cur: string) => U, init: U): U;
declare const fn: () => string;
declare const arr: string[];
const a: number = ov((v, k) => ({ [k]: v }) as any, fn);
const b: number = one((v, k) => ({ [k]: v }) as any, fn);
const c: number = ov((v, k) => v, fn);
const d: number = arr.reduceRight((v, k) => ({ [k]: v }) as any, fn);
const f: number = arr.reduce((v, k) => ({ [k]: v }) as any, fn);
const g: number = arr.reduce((v: any, k) => v, fn);

type ObjectLiteral = { [key: string]: any };
type QDP<T> = _QDP<ObjectLiteral extends T ? unknown : T>;
type _QDP<T> = {
  [P in keyof T]?: (T[P] extends Array<infer U> ? Array<_QDP<U>> : _QDP<T[P]>) | (() => string);
};
export function inc<Entity extends ObjectLiteral>(path: string, value: number): QDP<Entity> {
  const values: QDP<Entity> = path
    .split(".")
    .reduceRight((value, key) => ({ [key]: value }) as any, () => "x" + value);
  return values;
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "strictFunctionTypes": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true }, "files": ["a.ts"] }
