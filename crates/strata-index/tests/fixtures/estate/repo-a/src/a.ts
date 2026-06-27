// repo-a: a.ts — exports greet and calls a local helper so intra-repo impact works.

function helper(): string {
    return "hello";
}

export function greet(name: string): string {
    return helper() + " " + name;
}

export function farewell(name: string): string {
    return "goodbye " + name;
}
