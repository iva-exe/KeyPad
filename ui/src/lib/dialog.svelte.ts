// Otevřený potvrzovací dialog ↺ / 🗑 (Fáze 7, Z3) — jeden pro celé okno.
//
// Dřív se akce potvrzovala druhým klikem do 3 s; vlastník chce jeden
// klik a otázku. Dialog je vrstva ve WebView (Potvrzeni.svelte), ne okno
// Windows. Potvrzení jde stejnou cestou jako dřív (`uprav_klavesy`
// `vychozi`, `odeber_ovladac`) a backend podmínky ověří znovu.
import { odeberOvladac, prirazuje, vychozi, zrusPrirazeni } from './klavesy.svelte';
import type { DruhPotvrzeni } from './potvrzeni';

export const potvrzeni = $state({
	/** `null` = zavřeno. */
	druh: null as DruhPotvrzeni | null,
	pad: 0,
	/** Ovladač má klávesy — 🗑 je smaže (druhý řádek dialogu). */
	maKlavesy: false,
	/** Roste s každým otevřením. */
	id: 0
});

/** Ikona, ze které dialog vznikl — po zavření na ni vrátit fokus. */
let kotva: HTMLElement | null = null;

/**
 * Otevře dialog. Běžící přiřazování zruší (jako klik jinam) — otázka
 * a pulzující čepička naráz by si odporovaly.
 */
export function otevriPotvrzeni(druh: DruhPotvrzeni, pad: number, maKlavesy: boolean, odkud: HTMLElement | null): void {
	if (prirazuje()) zrusPrirazeni();
	kotva = odkud;
	potvrzeni.druh = druh;
	potvrzeni.pad = pad;
	potvrzeni.maKlavesy = maKlavesy;
	potvrzeni.id++;
}

/** Zavře bez akce (Zrušit, Esc, klik mimo, schované okno). */
export function zavriPotvrzeni(vratitFokus = true): void {
	if (potvrzeni.druh === null) return;
	potvrzeni.druh = null;
	const k = kotva;
	kotva = null;
	// Fokus zpět, odkud uživatel přišel — navigace Tabem pokračuje tam.
	// Ikona mohla mezitím zmizet (odebraný ovladač).
	if (vratitFokus && k?.isConnected) k.focus();
}

/** Potvrdí: zavře a provede akci (chybu ukáže nápověda karty). */
export async function potvrd(): Promise<void> {
	const { druh, pad } = potvrzeni;
	if (druh === null) return;
	zavriPotvrzeni();
	if (druh === 'vychozi') await vychozi();
	else await odeberOvladac(pad);
}
