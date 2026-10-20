// Keeping the promise in a variable and awaiting it later.
async function save(x: string): Promise<void> {
  console.log(x);
}
const pending = save("a");
console.log("working");
await pending;
export {};
