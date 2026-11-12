export function greet(name: string): string {
  return name;
}

export class Box {
  open(): void {}
  private seal(): void {}
}

export abstract class Base {
  abstract run(): void;
}

export interface Shape {
  area(): number;
}

export enum Color {
  Red,
}

export type Pair = [number, number];

export const twice = (n: number) => n * 2;

export namespace Util {
  export const v = 1;
}
