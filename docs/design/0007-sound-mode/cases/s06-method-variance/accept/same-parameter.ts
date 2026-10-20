// An object literal method with the same parameter type.
interface AppEvent {
  readonly type: string;
}
interface Handler {
  handle(e: AppEvent): void;
}
const h: Handler = {
  handle(e: AppEvent): void {
    console.log(e.type);
  },
};
h.handle({ type: "x" });
export {};
