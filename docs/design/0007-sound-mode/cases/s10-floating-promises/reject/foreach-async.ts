// forEach ignores the promises its callback returns.
async function save(x: string): Promise<void> {
  console.log(x);
}
const items: string[] = ["a", "b"];
items.forEach(async (i) => { await save(i); }); // @expect TSR1003
export {};
