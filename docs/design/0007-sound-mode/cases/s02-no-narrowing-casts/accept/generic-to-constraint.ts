// Casting a type parameter to its constraint is an upcast.
interface Named {
  readonly name: string;
}
function nameOf<T extends Named>(value: T): string {
  const named = value as Named;
  return named.name;
}
export const n: string = nameOf({ name: "a", extra: 1 });
