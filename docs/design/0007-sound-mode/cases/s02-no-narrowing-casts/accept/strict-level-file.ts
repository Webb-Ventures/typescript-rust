// @ts-r strict
// S-02 is a sound-level rule, so a file at the strict level may still narrow with a cast.
interface User {
  readonly id: number;
}
function load(data: unknown): User {
  return data as User;
}
export const u: User = load({ id: 1 });
