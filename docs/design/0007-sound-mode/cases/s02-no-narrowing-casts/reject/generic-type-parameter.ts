// unknown to a type parameter.
function cast<T>(x: unknown): T {
  return x as T; // @expect TSR0201
}
export const s: string = cast<string>(1);
