// A guard the checker can't prove, with a stated reason on the signature's line.
// @ts-r-unsafe S-07: ids come from the database, which enforces the user_ prefix.
function isUserId(x: string): x is `user_${string}` {
  return x.startsWith("user_");
}
export const ok: boolean = isUserId("user_1");
