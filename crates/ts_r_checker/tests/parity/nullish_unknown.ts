// unknown without null and undefined is {}; a generic left side of ?? is NonNullable<T>.
declare function from(c: Error | object): void;
export function f(cause: unknown) {
  from(cause ?? new Error('x'));
  const a = cause ?? new Error('x');
  const n: number = a;
  const b = cause!;
  const n2: number = b;
  let c = cause;
  c ??= 1;
  const n3: string = c;
}
export function g<T>(cause: T) {
  const a = cause ?? 1;
  const n: string = a;
}
