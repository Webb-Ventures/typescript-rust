export {};

declare global {
  interface Array<T> {
    lastItem(): T | undefined;
  }
  var APP_NAME: string;
}
