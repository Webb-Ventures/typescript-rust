// Awaiting the promise.
async function save(x: string): Promise<void> {
  console.log(x);
}
await save("a");
export {};
