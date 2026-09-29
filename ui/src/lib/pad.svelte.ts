// Stav virtuálního gamepadu pro celé okno (titulek i karta padu).
//
// Zdroj pravdy je backend (pad vlákno, src-tauri/src/platform/windows/
// pad.rs). Sem teče událostí `pad-stav` při každé změně a příkazem
// `pad_status` při startu a po návratu okna z oznamovací oblasti.
// Odpovědi a události se můžou předběhnout — pořadí drží `seq`.
import { poslouchej, textChyby, vAplikaci, zavolej } from './tauri';

export type PadStav =
	| 'connecting'
	| 'connected'
	/** ViGEmBus v systému vůbec není — jen tady se nabízí instalace. */
	| 'bus_missing'
	/** ViGEmBus je nainstalovaný, ale neběží — `detail` je rada, co s tím. */
	| 'bus_not_running'
	| 'error'
	| 'suspended';

/** Odpověď `pad_status` i obsah události `pad-stav`. */
interface PadInfo {
	state: PadStav;
	player: number | null;
	detail: string;
	seq: number;
}

/**
 * První adresa https v textu z backendu (chyba, rada), nebo `null`.
 *
 * Adresu ruční instalace ViGEmBus drží jen backend (`updater::vigembus`)
 * a posílá ji v textu — vlastní kopie tady by se s ní časem rozešla.
 * Koncová interpunkce věty k adrese nepatří.
 */
export function odkazV(text: string): string | null {
	const m = /https:\/\/[^\s)]+/.exec(text);
	return m ? m[0].replace(/[.,;:]+$/, '') : null;
}

export const pad = $state({
	state: 'connecting' as PadStav,
	/** Číslo hráče (slot XInput + 1), jen u připojeného padu. */
	player: null as number | null,
	/** Podrobnost od backendu (proč chyba, co se právě děje). */
	detail: '',
	/** Pořadí poslední převzaté změny; −1 = zatím nic. */
	seq: -1,
	/** Páčka zrovna opisuje kruh — tlačítko je chvíli zablokované. */
	testuje: false,
	/** Spouští se instalátor ViGEmBus. */
	spoustiInstalator: false,
	/** Instalátor běží; po jeho konci se backend zkusí připojit sám. */
	instalatorBezi: false,
	/** Chyba posledního kliknutí — ukáže se u tlačítek. */
	chybaAkce: ''
});

function prevezmi(i: PadInfo): void {
	// Starší změnu zahodit. Stejný `seq` projde: backend zapisuje
	// a čte celý stav najednou pod zámkem, takže stejný `seq` = stejný
	// obsah (odpověď `pad_status` po události se stejným číslem nic
	// nerozbije).
	if (i.seq < pad.seq) return;
	pad.state = i.state;
	pad.player = i.player ?? null;
	pad.detail = i.detail ?? '';
	pad.seq = i.seq;
	// Backend po konci instalátoru připojuje znovu — přes „připojuji…".
	// Tím instalátor skončil, ať už to dopadlo jakkoli.
	if (i.state !== 'bus_missing') pad.instalatorBezi = false;
	if (i.state !== 'connected') pad.testuje = false;
}

async function nacti(): Promise<void> {
	try {
		prevezmi(await zavolej<PadInfo>('pad_status'));
	} catch {
		// Mimo aplikaci backend není — zůstane „připojuji…".
	}
}

let spusteno = false;

/** Začne sledovat stav padu (idempotentní). */
export function startPad(): void {
	if (spusteno || !vAplikaci) return;
	spusteno = true;
	// Nejdřív poslouchat, pak se zeptat: změna mezi dotazem a přihlášením
	// k události by se jinak ztratila. Starší odpověď zahodí `seq`.
	void poslouchej<PadInfo>('pad-stav', prevezmi).then(nacti);
	// Schované okno má uspaný webview; po návratu stav pro jistotu znovu.
	document.addEventListener('visibilitychange', () => {
		if (!document.hidden) void nacti();
	});
}

/** Jak dlouho blokovat „Vyzkoušet páčku" — kruh trvá 1,2 s. */
const TEST_MS = 1300;

export async function vyzkousejPacku(): Promise<void> {
	if (pad.testuje) return;
	pad.chybaAkce = '';
	pad.testuje = true;
	try {
		await zavolej('pad_test');
		setTimeout(() => (pad.testuje = false), TEST_MS);
	} catch (e) {
		pad.chybaAkce = textChyby(e);
		pad.testuje = false;
	}
}

export async function zkusZnovu(): Promise<void> {
	pad.chybaAkce = '';
	try {
		await zavolej('pad_retry');
	} catch (e) {
		pad.chybaAkce = textChyby(e);
	}
}

export async function nainstalujVigem(): Promise<void> {
	if (pad.spoustiInstalator || pad.instalatorBezi) return;
	pad.chybaAkce = '';
	pad.spoustiInstalator = true;
	try {
		await zavolej('install_vigembus');
		pad.instalatorBezi = true;
	} catch (e) {
		pad.chybaAkce = textChyby(e);
	}
	pad.spoustiInstalator = false;
}
