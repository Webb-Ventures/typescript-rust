// A spread of a value whose type mentions a type parameter keeps it: the inferred return
// type of a generic function is instantiated at each call (tRPC's
// `testServerAndClientResource` returns `{ ...trpcServerResource(router), ... }`).
declare function mk<T>(t: T): { router: T };
export function v1<T>(router: T) {
  const s = mk(router);
  return { ...s, x: 1 };
}
export function v2<T>(router: T) {
  const s = { router };
  return { ...s, x: 1 };
}
export function v3<T>(router: T) {
  const s: { router: T } = mk(router);
  return { ...s };
}
export const n1: number = v1('a' as const).router;
export const n2: number = v2('a' as const).router;
export const n3: number = v3('a' as const).router;

// An instantiated literal with spreads still checks only its own written properties for
// excess (Playwright's config.ts: `[{ ...userConfig, workers: undefined }]` in a union of
// arrays, whose `map` instantiates the element type).
type UseOptions<T, W> = Partial<W> & Partial<T>;
interface Project<T = {}, W = {}> { name?: string; use?: UseOptions<T, W>; workers?: number | string }
interface Config<T = {}, W = {}> { projects?: Project<T, W>[]; name?: string; use?: UseOptions<T, W>; workers?: number }
declare const userConfig: Config;
declare const over: { projects?: { name: string; use?: any }[] };
declare function take(p: Project): void;
const pc = over.projects || [{ ...userConfig, workers: undefined }];
pc.map(p => take(p));
