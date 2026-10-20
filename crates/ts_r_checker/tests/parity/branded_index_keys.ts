// date-fns `pkgs/docs/src/json.ts` (typedoc's `ReflectionId`, `NormalizedPath`): a branded
// primitive (`number & { __brand: never }`) indexes as its primitive.
declare const __normPath: unique symbol;
type NormalizedPath = "" | "/" | (string & { readonly __normPath: typeof __normPath });
type Id = number & { __brand: never };
declare const id: Id;
declare const p: NormalizedPath;
const m: Record<number, string> = {};
const s: Record<string, number> = {};
m[id] = "";
const x = m[id];
const y = s[p];
declare const kinds: Record<string, { kind: string }>;
const fileName: NormalizedPath | undefined = p;
const o = (fileName && kinds[fileName]) || undefined;
const bad = s[{} as { a: 1 }];
export {};
