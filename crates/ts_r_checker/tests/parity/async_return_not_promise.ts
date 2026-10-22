export class Task<T> extends Promise<T> {}
export async function a(): Task<number> {
  return 1;
}
export async function b(): PromiseLike<void> {}
export async function c(): number {}
export async function d(): Promise<string> {
  return "";
}
export class C {
  async m<T>(x: T): Task<T> {
    return x;
  }
}
