// The promise is dropped; a rejection would be unhandled.
async function save(x: string): Promise<void> {
  console.log(x);
}
save("a"); // @expect TSR1001
export {};
