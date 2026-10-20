// Asserting an awaited result.
async function firstLine(read: () => Promise<string | null>): Promise<string> {
  const text = (await read())!; // @expect TSR0301
  return text;
}
export { firstLine };
