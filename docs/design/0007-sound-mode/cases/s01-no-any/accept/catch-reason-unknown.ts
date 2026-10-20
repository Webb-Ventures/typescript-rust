// Promise.catch's reason is any in lib; annotating it unknown and narrowing is sound.
function describe(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
async function run(task: () => Promise<string>): Promise<string> {
  return task().catch((e: unknown) => describe(e));
}
export { run };
