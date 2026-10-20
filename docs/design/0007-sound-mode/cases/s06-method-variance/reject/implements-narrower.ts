// A class method that only accepts a subtype of the interface's parameter.
interface AppEvent {
  readonly type: string;
}
interface ClickEvent extends AppEvent {
  readonly x: number;
}
interface Handler {
  handle(e: AppEvent): void;
}
class ClickOnly implements Handler {
  handle(e: ClickEvent): void { // @expect TSR0601
    console.log(e.x);
  }
}
new ClickOnly().handle({ type: "click", x: 1 });
export {};
