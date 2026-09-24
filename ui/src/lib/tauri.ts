// Jediné místo, kudy frontend mluví s backendem.
//
// UI se musí dát otevřít i v obyčejném prohlížeči (`bun run dev` bez
// Tauri) — ladí se tak rozložení a styly bez buildu Rustu. Tam ale
// `window.__TAURI_INTERNALS__` neexistuje a každé přímé `invoke` nebo
// `getCurrentWindow()` by spadlo výjimkou už při načtení. Proto jde
// všechno přes tenhle modul, který se nejdřív zeptá, kde běží.
import { invoke, isTauri, type InvokeArgs } from '@tauri-apps/api/core';
import { getCurrentWindow, type Window } from '@tauri-apps/api/window';

/** Běží stránka uvnitř aplikace (ne v prohlížeči)? */
export const vAplikaci: boolean = isTauri();

/**
 * Zavolá příkaz backendu. Mimo aplikaci vrátí zamítnutý slib s českou
 * hláškou — volající s chybou počítá tak jako tak (síť, backend), takže
 * nemusí rozlišovat „prohlížeč" jako zvláštní případ.
 */
export function zavolej<T>(prikaz: string, args?: InvokeArgs): Promise<T> {
	if (!vAplikaci) {
		return Promise.reject(new Error('běží v prohlížeči — backend aplikace tu není'));
	}
	return invoke<T>(prikaz, args);
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
