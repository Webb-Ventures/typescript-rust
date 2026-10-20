// Fire and forget with a rejection handler.
async function save(x: string): Promise<void> {
  console.log(x);
}
function report(e: unknown): void {
  console.error(e);
}
void save("a").catch(report);
export {};
