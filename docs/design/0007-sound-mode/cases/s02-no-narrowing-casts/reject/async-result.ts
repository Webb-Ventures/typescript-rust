// Narrowing an awaited unknown.
async function count(get: () => Promise<unknown>): Promise<number> {
  const value = await get();
  return value as number; // @expect TSR0201
}
export { count };
