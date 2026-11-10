// A type alias resolved through `typeof` of a value (zod 4's `z.infer<typeof Schema>`, the
// React Compiler's `EnvironmentConfig`) checks the value's initializer and the library
// signatures it calls. An alias named there and not yet known (`Strip`, zod's `$strip`) is
// resolved on the spot, not handed out as a forward reference: the constraint check of
// `opt(Hooks)` reads `Strip["out"]`, and a forward read as `any` there made every config
// `{ [x: string]: any }` (TS4111 on each property under noPropertyAccessFromIndexSignature).
import { num, object, opt, str, type infer as Infer } from './lib';

declare const c: Config;
export const n: number = c.hooks.a;
export const s: string = c.name;

const Hooks = object({ a: num() });
export const Schema = object({ hooks: opt(Hooks), name: str() });
export type Config = Infer<typeof Schema>;
// @filename: lib.d.ts
export type Strip = { out: {} };
type Flat<T> = { [K in keyof T]: T[K] };
export type Infer<S, E> = keyof (S & E) extends never
  ? Record<string, never>
  : Flat<{ -readonly [K in keyof S]: S[K] extends { _out: infer O } ? O : never } & E>;
export interface Internals<S, C extends { out: object }> {
  output: Infer<S, C['out']>;
}
export interface Obj<S, C extends { out: object } = Strip> {
  _out: Internals<S, C>['output'];
  _zod: Internals<S, C>;
}
export interface Num {
  _out: number;
}
export interface Str {
  _out: string;
}
export declare function object<S>(shape: S): Obj<S, Strip>;
export declare function num(): Num;
export declare function str(): Str;
export declare function opt<V extends { _out: object }>(v: V): V;
export type infer<T extends { _zod: { output: unknown } }> = T['_zod']['output'];
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "noUncheckedIndexedAccess": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "skipLibCheck": true, "noPropertyAccessFromIndexSignature": true }, "files": ["a.ts", "lib.d.ts"] }
