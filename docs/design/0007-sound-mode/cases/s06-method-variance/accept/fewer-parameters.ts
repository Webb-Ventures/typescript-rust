// A method that ignores its parameter.
interface AppEvent {
  readonly type: string;
}
interface Handler {
  handle(e: AppEvent): void;
}
const h: Handler = {
  handle(): void {
    console.log("handled");
  },
};
h.handle({ type: "x" });
export {};
