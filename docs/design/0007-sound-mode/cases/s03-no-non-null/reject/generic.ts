// Asserting a generic value is defined.
function must<T>(value: T | undefined): T {
  return value!; // @expect TSR0301
}
export const n: number = must<number>(1);
