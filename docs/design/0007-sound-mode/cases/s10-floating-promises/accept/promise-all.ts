// An array of promises passed to Promise.all and awaited.
async function save(x: string): Promise<void> {
  console.log(x);
}
const items: string[] = ["a", "b"];
await Promise.all(items.map(async (i) => save(i)));
export {};
