// then with a rejection handler, and catch on its own, count as handled.
async function save(x: string): Promise<void> {
  console.log(x);
}
function report(e: unknown): void {
  console.error(e);
}
save("a").then(() => console.log("saved"), report);
save("b").catch(report);
export {};
