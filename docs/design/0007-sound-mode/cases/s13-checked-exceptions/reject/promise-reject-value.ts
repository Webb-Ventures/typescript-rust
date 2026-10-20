// Promise.reject with a non-Error value.
function load(id: string): Promise<number> {
  if (id === "") return Promise.reject("empty id"); // @expect TSR1303
  return Promise.resolve(id.length);
}
export const p: Promise<number> = load("a");
