// Jediné místo, kudy frontend mluví s backendem.
//
// UI se musí dát otevřít i v obyčejném prohlížeči (`bun run dev` bez
// Tauri) — ladí se tak rozložení a styly bez buildu Rustu. Tam ale
// `window.__TAURI_INTERNALS__` neexistuje a každé přímé `invoke` nebo
// `getCurrentWindow()` by spadlo výjimkou už při načtení. Proto jde
// všechno přes tenhle modul, který se nejdřív zeptá, kde běží.
import { invoke, isTauri, type InvokeArgs } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow, type Window } from '@tauri-apps/api/window';

/** Běží stránka uvnitř aplikace (ne v prohlížeči)? */
export const vAplikaci: boolean = isTauri();

/**
 * Maketa backendu (lib/maketa.ts) — jen `bun run dev` v prohlížeči, aby
 * šlo okno ladit s daty bez buildu Rustu. V buildu je
 * `import.meta.env.DEV` konstanta `false`, takže Rollup tuhle větev
 * i s modulem zahodí a maketa se do binárky nedostane.
 */
const maketa = import.meta.env.DEV && !vAplikaci ? import('./maketa') : null;

/** Je s kým mluvit (aplikace, nebo maketa ve vývoji)? */
export const maBackend: boolean = vAplikaci || maketa !== null;

/**
 * Zavolá příkaz backendu. Mimo aplikaci vrátí zamítnutý slib s českou
 * hláškou — volající s chybou počítá tak jako tak (síť, backend), takže
 * nemusí rozlišovat „prohlížeč" jako zvláštní případ.
 */
export function zavolej<T>(prikaz: string, args?: InvokeArgs): Promise<T> {
	if (vAplikaci) return invoke<T>(prikaz, args);
	if (maketa) return maketa.then((m) => m.zavolej<T>(prikaz, args as Record<string, unknown> | undefined));
	return Promise.reject(new Error('běží v prohlížeči — backend aplikace tu není'));
}

/**
 * Poslouchá událost z backendu (stav padu…). Mimo aplikaci nic —
 * události tam nechodí. Oprávnění: core:event:allow-listen a
 * allow-unlisten v src-tauri/capabilities/default.json.
 */
export async function poslouchej<T>(udalost: string, obsluha: (data: T) => void): Promise<UnlistenFn> {
	if (vAplikaci) return listen<T>(udalost, (e) => obsluha(e.payload));
	if (maketa) return (await maketa).poslouchej(udalost, (d) => obsluha(d as T));
	return () => {};
}

let okno: Window | null = null;

/** Hlavní okno, nebo `null` mimo aplikaci. Vytváří se líně a jednou. */
export function hlavniOkno(): Window | null {
	if (!vAplikaci) return null;
	okno ??= getCurrentWindow();
	return okno;
}

/** Text chyby z `invoke` — Rust vrací prostý řetězec, JS výjimku. */
export function textChyby(e: unknown): string {
	if (e instanceof Error) return e.message;
	return String(e);
}
