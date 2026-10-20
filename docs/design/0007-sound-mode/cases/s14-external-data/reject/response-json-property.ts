// Reading a property off res.json() without parsing.
async function total(res: Response): Promise<number> {
  const data = await res.json();
  return data.total; // @expect TSR1401
}
export { total };
