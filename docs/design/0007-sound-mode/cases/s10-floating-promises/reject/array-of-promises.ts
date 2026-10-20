// An array of promises as a statement.
async function save(x: string): Promise<void> {
  console.log(x);
}
const items: string[] = ["a", "b"];
items.map((i) => save(i)); // @expect TSR1001
export {};
