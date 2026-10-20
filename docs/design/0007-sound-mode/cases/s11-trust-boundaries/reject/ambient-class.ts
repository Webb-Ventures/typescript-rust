// declare class in a sound file.
declare class Widget { // @expect TSR1103
  render(): string;
}
export const html: string = new Widget().render();
