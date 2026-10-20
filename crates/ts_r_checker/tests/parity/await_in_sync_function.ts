declare function load(): Promise<string>;
export function f() {
  const s = await load();
  return s;
}
export const g = (): string => {
  return await load();
};
export class C {
  m() {
    return await load();
  }
}
export async function h() {
  const inner = () => await load();
  return inner;
}
