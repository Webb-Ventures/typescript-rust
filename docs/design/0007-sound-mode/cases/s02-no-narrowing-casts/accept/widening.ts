// never[] to string[], null to a nullable union, and anything to unknown are all upcasts.
const names = [] as string[];
const maybe = null as string | null;
function erase<T>(value: T): unknown {
  return value as unknown;
}
names.push("a");
export const out: unknown = erase(maybe);
