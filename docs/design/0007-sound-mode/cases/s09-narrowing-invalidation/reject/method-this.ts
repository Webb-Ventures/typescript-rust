// A method call on this can change this.track.
class Player {
  track: string | null = null;
  next(): void {
    this.track = null;
  }
  play(): number {
    if (this.track !== null) {
      this.next();
      return this.track.length; // @expect TSR0901
    }
    return 0;
  }
}
export const n: number = new Player().play();
