// Stav aktualizace pro celou aplikaci — port WinSentova updater store.
//
// Bydlí v modulu, ne v komponentě: banner s novou verzí ho kreslí dole
// v okně a časem ho bude chtít i obrazovka nastavení. Dvě kopie stavu
// by znamenaly dvě různá čísla na jedné obrazovce.
import { textChyby, vAplikaci, zavolej } from './tauri';

/** Odpověď příkazu `check_update` (viz src-tauri/src/update.rs). */
interface UpdateInfo {
	current: string;
	latest: string;
	available: boolean;
	error: string | null;
}

export const updater = $state({
	/** Verze, která běží. Prázdná = vývojový build bez version.txt. */
	current: '',
	/** Verze v repozitáři; prázdná, dokud se nepodařilo zjistit. */
	latest: '',
	/** Je co aktualizovat? */
	available: false,
	/** Poslední kontrola (ms epocha) — `null` = ještě neproběhla. */
	checkedAt: null as number | null,
	/** Proč kontrola nevyšla. Banner to nekřičí — v logu a časem
	    v nastavení to ale vidět má. */
	error: '',
	/** Probíhá aktualizace (stahuje se instalátor, nebo už běží)? */
	busy: false,
	/** Instalátor je stažený a spuštěný — teď je řada na něm. */
	launched: false,
	/** Chyba spuštění aktualizace — tahle se v banneru ukazuje, protože
	    na ni uživatel právě kliknul a čeká výsledek. */
	runError: '',
	/** Právě se ptáme repozitáře. Kontrola jde po síti a může trvat;
	    bez příznaku by se při souběhu dotazy vršily na sebe. */
	checking: false
});

export async function checkUpdate(): Promise<void> {
	if (updater.checking) return;
	updater.checking = true;
	try {
		const r = await zavolej<UpdateInfo>('check_update');
		updater.current = r.current ?? '';
		updater.error = r.error ?? '';
		// Nepovedená kontrola (výpadek sítě, GitHub) o verzi v repozitáři
		// nic neříká — platí poslední známý stav. Jinak by banner při
		// každém zakolísání Wi-Fi zajel a za minutu zase vyjel a vzal
		// s sebou i chybu aktualizace, kterou uživatel zrovna čte.
		if (!r.error) {
			updater.latest = r.latest ?? '';
			updater.available = !!r.available;
		}
	} catch (e) {
		updater.error = textChyby(e);
	}
	updater.checkedAt = Date.now();
	updater.checking = false;
}

/**
 * Jak dlouho po spuštění instalátoru se čeká, než se tlačítko uvolní.
 *
 * Normálně instalátor tohle okno do pár sekund zavře a aplikaci spustí
 * znovu — čas vyprší s ním. Když ale selže dřív, než okno zavře (síť,
 * GitHub), hlášku ukazuje ve svém okně a tady by tlačítko navždy
 * viselo. Minuta a půl stačí i na pomalé stahování.
 */
const INSTALATOR_MS = 90 * 1000;

let uvolneni: ReturnType<typeof setTimeout> | undefined;

export async function runUpdate(): Promise<void> {
	if (updater.busy) return;
	updater.busy = true;
	updater.launched = false;
	updater.runError = '';
	clearTimeout(uvolneni);
	try {
		// Instalátor tohle okno sám zavře, přepíše binárku a aplikaci
		// spustí znovu. Odpověď „hotovo" tedy nepřijde — okno zmizí
		// dřív. `busy` proto zůstává zapnuté, dokud nevyprší pojistka.
		await zavolej<string>('run_update');
		updater.launched = true;
		uvolneni = setTimeout(() => {
			updater.busy = false;
			updater.launched = false;
		}, INSTALATOR_MS);
	} catch (e) {
		updater.runError = textChyby(e);
		updater.busy = false;
	}
}

/**
 * Jak často se ptát repozitáře na novou verzi.
 *
 * Minuta stačí: `raw.githubusercontent.com` drží soubor v CDN cache
 * pět minut, takže častější dotaz by novou verzi stejně neviděl dřív.
 * A jeden malý soubor za minutu je proti tomu levný.
 */
const INTERVAL_MS = 60 * 1000;

let timer: ReturnType<typeof setInterval> | undefined;

/** Spustí kontrolu (idempotentní). Při startu hned, pak v intervalu. */
export function startUpdateChecks(): void {
	// V prohlížeči není backend — kontrola by jen každou minutu
	// zapsala stejnou chybu.
	if (timer || !vAplikaci) return;
	void checkUpdate();
	timer = setInterval(() => void checkUpdate(), INTERVAL_MS);
}
