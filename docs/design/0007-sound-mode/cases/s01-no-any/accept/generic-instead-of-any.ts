// A type parameter replaces Record<string, any>.
function values<T>(record: Readonly<Record<string, T>>): T[] {
  return Object.values(record);
}
export const nums: number[] = values({ a: 1, b: 2 });
