// trpc `resolveResponse.ts` (`const infoTuple: ResultTuple<TRPCRequestInfo> = await run(async
// () => { try { return [undefined, await getRequestInfo(...)]; } catch { return [err,
// undefined]; } })`): a function argument's contextual return type that is a type parameter
// of the call reads as what the call's contextual type implies for it, and `await x` gives
// `x` the context `T | PromiseLike<T>`.
type ResultTuple<T> = [undefined, T] | [Error, undefined];
const run = <TValue>(fn: () => TValue): TValue => fn();
declare function run2<TValue>(fn: () => TValue): TValue;
declare function info(): Promise<{ a: number }>;
async function f() {
  const t1: ResultTuple<{ a: number }> = await run(async () => {
    try {
      return [undefined, await info()];
    } catch (cause) {
      return [new Error(), undefined];
    }
  });
  const t2: ResultTuple<{ a: number }> = run(() => {
    return [undefined, { a: 1 }];
  });
  const t3: ResultTuple<{ a: number }> = await run(async () => [undefined, { a: 1 }]);
  const t4: [undefined, number] = run2(() => [undefined, 1]);
  const t5: Array<[string, number]> = run(() => [1, 2].map((x) => ["a", x]));
  const t6: { k: "a" | "b" } = run(() => ({ k: "a" }));
  const t7: [string, number] = run2(() => ["a", "b"]);
  const t8: { k: "a" | "b" } = await run(async () => ({ k: "c" }));
}
// trpc `resolveResponse.ts` (`const type = call?.procedure?._def.type ?? 'unknown';
// opts.onError?.({ ..., type })`): a shorthand property's value is contextually typed like
// `type: type`, so its literal types are kept.
type ProcedureType = "query" | "mutation";
declare function g(o: { type: ProcedureType | "unknown" }): void;
declare const call: { t: ProcedureType | null } | undefined;
const type = call?.t ?? "unknown";
g({ type });
let other = Math.random() ? "query" : "x";
g({ type: other });
export {};
