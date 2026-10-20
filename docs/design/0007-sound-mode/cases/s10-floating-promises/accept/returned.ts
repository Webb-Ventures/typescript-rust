// Returning the promise hands it to the caller.
async function save(x: string): Promise<void> {
  console.log(x);
}
function saveAll(): Promise<void> {
  return save("a");
}
await saveAll();
export {};
