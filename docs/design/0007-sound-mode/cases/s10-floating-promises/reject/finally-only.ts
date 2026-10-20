// finally doesn't handle a rejection.
async function save(x: string): Promise<void> {
  console.log(x);
}
save("a").finally(() => console.log("done")); // @expect TSR1001
export {};
