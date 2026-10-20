// Assigning an instance whose method parameter is narrower.
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
const h: Handler = new ClickLogger(); // @expect TSR0601
h.handle({ type: "key" });
export {};
