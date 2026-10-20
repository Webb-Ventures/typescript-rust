// A function argument with no unannotated parameters inside is typed by its first
// contextual type, with the call's type parameters still being inferred.
type Offline = { error: { code: 'NETWORK' } };
declare function attempt<T>(call: Promise<T>): Promise<T | Offline>;
declare const api: any;
declare function cus<R extends { data?: number }>(check: () => Promise<R>, onPending: (result: R) => void): Promise<R>;
declare function cus2<R>(check: () => Promise<R>, onPending: (result: R) => void): Promise<R>;
declare function cus3<R extends { data?: number }>(check: () => Promise<R>): Promise<R>;
declare function cus4<R extends number>(check: () => Promise<R>, onPending: (result: R) => void): Promise<R>;
export async function f() {
  const a = await cus(() => attempt(api), (p) => {});
  const b = await cus2(() => attempt(api), (p) => {});
  const c = await cus3(() => attempt(api));
  const d = await cus4(() => attempt(api), (p) => {});
  const x: Promise<{ a: 1 }> = attempt(api);
  const na: string = a;
  const nb: string = b;
  const nc: string = c;
  const nd: string = d;
}
