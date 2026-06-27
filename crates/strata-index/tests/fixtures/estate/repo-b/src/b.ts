// repo-b: b.ts — exports compute and process.

export function compute(x: number): number {
    return x * 2;
}

export function process(items: number[]): number[] {
    return items.map(compute);
}
