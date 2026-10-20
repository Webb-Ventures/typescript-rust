// An object literal method with a narrower parameter.
interface AppEvent {
  readonly type: string;
}
interface ClickEvent extends AppEvent {
  readonly x: number;
}
interface Handler {
  handle(e: AppEvent): void;
}
const h: Handler = { handle(e: ClickEvent): void { console.log(e.x); } }; // @expect TSR0601
h.handle({ type: "key" });
export {};
