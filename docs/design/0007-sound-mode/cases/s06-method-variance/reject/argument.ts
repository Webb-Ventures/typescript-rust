// Passing a narrower handler as an argument.
interface AppEvent {
  readonly type: string;
}
interface ClickEvent extends AppEvent {
  readonly x: number;
}
interface Handler {
  handle(e: AppEvent): void;
}
class ClickLogger {
  handle(e: ClickEvent): void {
    console.log(e.x);
  }
}
function register(h: Handler): void {
  h.handle({ type: "key" });
}
register(new ClickLogger()); // @expect TSR0601
export {};
