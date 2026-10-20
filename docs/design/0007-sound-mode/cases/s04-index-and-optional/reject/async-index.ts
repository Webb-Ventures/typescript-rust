// An awaited array read as if it were non-empty.
async function firstId(load: () => Promise<number[]>): Promise<number> {
  const ids = await load();
  return ids[0]; // @expect TSR0401
}
export { firstId };
