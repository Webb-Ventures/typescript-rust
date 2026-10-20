// Promise.catch gives the rejection reason type any in lib.
async function load(task: () => Promise<string>): Promise<string> {
  return task().catch((e) => e.message); // @expect TSR0102
}
export { load };
