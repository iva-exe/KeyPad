// Typy pro `bun:test` a `bun` — jen to, co testy okna opravdu používají.
//
// Proč ne `@types/bun`: přitáhl by do node_modules typy celého Bunu
// a Node (a s nimi globální `setTimeout` vracející objekt místo čísla)
// kvůli pár funkcím v testech. Testy spouští Bun, který typy nekontroluje;
// tohle je jen pro svelte-check, aby kontroloval i testy. Kdyby se
// `@types/bun` někdy přidal, tenhle soubor smazat.
declare module 'bun:test' {
	interface Shoda {
		toBe(ocekavano: unknown): void;
		toEqual(ocekavano: unknown): void;
		toBeNull(): void;
		toBeUndefined(): void;
		toBeCloseTo(ocekavano: number, desetinnych?: number): void;
		toContain(prvek: unknown): void;
		toHaveLength(delka: number): void;
		toMatch(vzor: RegExp | string): void;
		toBeLessThan(mez: number): void;
		not: Shoda;
	}
	export function describe(nazev: string, telo: () => void): void;
	export function test(nazev: string, telo: () => void | Promise<void>): void;
	export function expect(hodnota: unknown): Shoda;
	export const mock: {
		/** Nahradí modul (cesta relativně k testu) — volat před jeho importem. */
		module(cesta: string, tovarna: () => Record<string, unknown>): void;
	};
}

// Předehra testů (src/bun-svelte.ts).
declare module 'bun' {
	interface Nacteni {
		path: string;
	}
	interface Stavitel {
		onLoad(
			volby: { filter: RegExp },
			obsluha: (n: Nacteni) => Promise<{ contents: string; loader: 'js' }>
		): void;
	}
	export function plugin(p: { name: string; setup(build: Stavitel): void }): void;
	export class Transpiler {
		constructor(volby: { loader: 'ts' });
		transformSync(kod: string): string;
	}
	export function file(cesta: string): { text(): Promise<string> };
}
