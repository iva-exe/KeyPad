<script lang="ts">
	import Download from 'lucide-svelte/icons/download';
	import ExternalLink from 'lucide-svelte/icons/external-link';
	import Gamepad2 from 'lucide-svelte/icons/gamepad-2';
	import Play from 'lucide-svelte/icons/play';
	import RotateCw from 'lucide-svelte/icons/rotate-cw';
	import { slide } from 'svelte/transition';
	import { aplikace } from './aplikace.svelte';
	import { trvani, zpomaleni } from './motion';
	import { instalujOvladac, otevri, pad, prepni, vyzkousej, zapnuto, zkusZnovu } from './pad.svelte';
	import Prepinac from './Prepinac.svelte';

	// Karta ovladače: jméno, tečka stavu, přepínač — a pod tím nejvýš
	// krátká věta a tlačítka, která v daném stavu něco udělají. Kódy
	// chyb a vysvětlivky patří do bubliny a logu, ne do okna.
	// Nic se nepředstírá: „zapnutý" až po potvrzení z ovladače.

	/** Přepínač ukazuje přání uživatele, dokud backend neodpoví. */
	const zapnutyPrepinac = $derived(pad.prepina ? pad.chce : zapnuto());

	// Bez ViGEmBus není co zapnout — místo přepínače mluví tlačítko.
	// Během instalace ovladače taky ne: instalátor by pad odebral.
	const lzePrepnout = $derived(
		pad.state !== 'bus_missing' &&
			pad.state !== 'bus_not_running' &&
			!pad.instalatorBezi &&
			!pad.spoustiInstalator
	);

	const veta = $derived.by(() => {
		if (pad.instalatorBezi) return 'Instalátor běží…';
		switch (pad.state) {
			case 'off':
				// Věta v kartě zůstává krátká (celá by se v úzkém okně
				// utnula); proč se nezapnul a co s tím, řekne bublina.
				return pad.detail ? 'Nezapnul se' : 'Vypnutý';
			case 'connecting':
				return 'Zapínám…';
			case 'on':
				// Bez čísla hráče: ViGEmBus ho s víc pady hlásí špatně.
				return pad.rezim === 'paused' ? 'Pozastaveno' : 'Zapnutý';
			case 'bus_missing':
				return 'Chybí ViGEmBus';
			case 'bus_not_running':
				return 'ViGEmBus neběží';
			case 'error':
				return 'Nefunguje';
		}
	});

	// Podrobnosti do bubliny: důvod chyby, rada k neběžícímu ViGEmBus
	// (tatáž jako v instalátoru), co udělá instalace.
	const bublina = $derived.by(() => {
		if (pad.instalatorBezi) return 'Běží instalátor ViGEmBus — ovladač půjde zapnout po jeho konci';
		switch (pad.state) {
			case 'off':
				// Backend posílá podrobnost jen k odmítnutému zapnutí
				// („počítač se uspává…", „ViGEmBus se právě spustil…").
				return pad.detail || 'Virtuální ovladač Xbox 360 — zapne ho přepínač';
			case 'connecting':
				return pad.detail || 'Zapínám virtuální ovladač Xbox 360';
			case 'on':
				return pad.rezim === 'paused'
					? 'Klávesy jdou do Windows — Scroll Lock ovladač zase pustí'
					: 'Klávesy ovládají virtuální ovladač — Scroll Lock je vrátí Windows';
			case 'bus_missing':
				return 'Bez ovladače ViGEmBus virtuální gamepad nevznikne. Instalaci potvrdíš ve výzvě Windows.';
			case 'bus_not_running':
			case 'error':
				return pad.detail;
		}
	});

	// Aktualizace staršího ViGEmBus v jakémkoli stavu ovladače: zapnutý
	// backend před spuštěním instalátoru sám vypne (neutrál → odpojit)
	// a přepínač zablokuje až do konce instalátoru.
	const instalace = $derived(pad.state === 'bus_missing' && aplikace.setup);
	const aktualizace = $derived(pad.stary && pad.state !== 'bus_missing' && aplikace.setup);
	const bublinaInstalace = $derived(
		instalace
			? 'Oficiální instalátor ViGEmBus — Windows se zeptají na oprávnění správce'
			: zapnuto()
				? 'Novější ViGEmBus — ovladač se nejdřív vypne; Windows se zeptají na oprávnění správce'
				: 'Novější ViGEmBus — Windows se zeptají na oprávnění správce'
	);
	// Ruční stažení: KeyPad neběží z instalace (instalátor ViGEmBus tu
	// není), nebo rada „zbytek ovladače" má adresu vydání. Tlačítko
	// otevře pevnou stránku (backend), žádnou adresu z textu.
	const rucne = $derived(
		(pad.state === 'bus_not_running' && /https:\/\//.test(pad.detail)) ||
			((pad.state === 'bus_missing' || pad.stary) && !aplikace.setup)
	);
	const znovu = $derived(
		pad.state === 'error' || pad.state === 'bus_not_running' || pad.state === 'bus_missing'
	);

	let chybaOdkazu = $state('');
	async function stahni() {
		chybaOdkazu = await otevri('vigembus_releases');
	}
	const chyba = $derived(pad.chybaAkce || chybaOdkazu);
</script>

<!-- data-seq: pořadí poslední převzaté změny — podle něj jde zvenku
     (test přes CDP) poznat, že dorazila událost, ne jen odpověď. -->
<section
	class="card"
	data-stav={pad.state}
	data-rezim={pad.rezim}
	data-seq={pad.seq}
	aria-live="polite"
>
	<div class="radek">
		<span class="ikona"><Gamepad2 size={18} strokeWidth={1.75} /></span>
		<div class="text">
			<span class="jmeno">Ovladač</span>
			<span class="veta" title={bublina}>{veta}</span>
		</div>
		<span class="dot" title={bublina}></span>
		<Prepinac
			zapnuto={zapnutyPrepinac}
			zakazano={!lzePrepnout}
			ceka={pad.prepina}
			popis="Virtuální ovladač"
			title={zapnutyPrepinac ? 'Vypnout ovladač' : 'Zapnout ovladač'}
			onprepni={() => void prepni()}
		/>
	</div>

	{#if pad.state === 'on' || instalace || aktualizace || znovu || rucne}
		<div class="akce" transition:slide={{ duration: trvani(140), easing: zpomaleni }}>
			{#if pad.state === 'on'}
				<button
					class="btn"
					disabled={pad.testuje}
					title="Levá páčka opíše kruh — vidět je třeba v joy.cpl"
					onclick={() => void vyzkousej()}
				>
					<Play size={13} strokeWidth={1.9} />
					{pad.testuje ? 'Zkouším…' : 'Vyzkoušet'}
				</button>
			{/if}
			{#if instalace || aktualizace}
				<button
					class="btn"
					class:primary={instalace}
					disabled={pad.spoustiInstalator || pad.instalatorBezi}
					title={bublinaInstalace}
					onclick={() => void instalujOvladac()}
				>
					<Download size={14} strokeWidth={1.9} />
					{instalace ? 'Nainstalovat ovladač' : 'Aktualizovat ovladač'}
				</button>
			{/if}
			{#if rucne}
				<button class="btn" title="Stránka vydání ViGEmBus v prohlížeči" onclick={() => void stahni()}>
					<ExternalLink size={13} strokeWidth={1.9} />
					Stáhnout ovladač
				</button>
			{/if}
			{#if znovu}
				<button class="btn" disabled={pad.instalatorBezi} onclick={() => void zkusZnovu()}>
					<RotateCw size={13} strokeWidth={1.9} />
					Zkusit znovu
				</button>
			{/if}
		</div>
	{/if}

	{#if chyba}
		<p class="err selectable" transition:slide={{ duration: trvani(140), easing: zpomaleni }}>
			{chyba}
		</p>
	{/if}
</section>

<style>
	.card {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		padding: 0.85rem 0.9rem 0.85rem 1rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		transition: border-color var(--t-fast) var(--ease);
	}
	/* Barva jen podle významu: zelený nádech teprve u skutečně
	   zapnutého ovladače, červený u poruchy. */
	.card[data-stav='on'] {
		border-color: color-mix(in srgb, var(--ok) 28%, var(--border));
	}
	.card[data-stav='bus_missing'],
	.card[data-stav='error'] {
		border-color: color-mix(in srgb, var(--danger) 32%, var(--border));
	}
	/* Neběžící ViGEmBus není porucha KeyPadu, ale úkol pro uživatele
	   (zapnout, restartovat) — jantarová, ne červená. */
	.card[data-stav='bus_not_running'] {
		border-color: color-mix(in srgb, var(--warn) 32%, var(--border));
	}

	.radek {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.ikona {
		display: grid;
		place-items: center;
		flex: none;
		color: var(--text-dim);
	}
	.text {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}
	.jmeno {
		font-size: var(--fs-xl);
		font-weight: 500;
		color: var(--text);
	}
	.veta {
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		letter-spacing: 0.03em;
		color: var(--text-faint);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		transition: color var(--t-fast) var(--ease);
	}
	.card[data-stav='on'] .veta {
		color: var(--text-dim);
	}
	.card[data-stav='bus_missing'] .veta,
	.card[data-stav='error'] .veta {
		color: var(--danger);
	}
	.card[data-stav='bus_not_running'] .veta {
		color: var(--warn);
	}

	/* Tečka stavu: zelená jen tehdy, když ovladač opravdu existuje
	   a přijal neutrál (WinSent: zelená nikdy bez skutečnosti). Šedé
	   tečky nesvítí — glow patří jen významovým barvám. */
	.dot {
		flex: none;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--text-faint);
		transition:
			background var(--t-fast) var(--ease),
			box-shadow var(--t-fast) var(--ease);
	}
	.card[data-stav='connecting'] .dot {
		background: var(--text-dim);
		animation: pulz 1.6s ease-in-out infinite;
	}
	.card[data-stav='on'] .dot {
		background: var(--ok);
		box-shadow: var(--glow-ok);
	}
	.card[data-stav='bus_missing'] .dot,
	.card[data-stav='error'] .dot {
		background: var(--danger);
		box-shadow: var(--glow-danger);
	}
	.card[data-stav='bus_not_running'] .dot {
		background: var(--warn);
		box-shadow: var(--glow-warn);
	}
	/* Pozastaveno zkratkou: ovladač existuje (zelená zůstává pravdivá),
	   ale klávesy teď jdou do Windows — tečka zhasne na obrys, ať je
	   rozdíl vidět na první pohled bez čtení. */
	.card[data-stav='on'][data-rezim='paused'] .dot {
		background: transparent;
		box-shadow: inset 0 0 0 1.5px var(--ok);
	}
	/* Zapínání: jemné pulzování — je vidět, že se něco děje, ale
	   nekřičí. „Omezit pohyb" ve Windows ho vypne (app.css). */
	@keyframes pulz {
		0%,
		100% {
			opacity: 0.35;
		}
		50% {
			opacity: 1;
		}
	}

	.akce {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		/* Pod textem, ne pod ikonou — řádek pak drží jednu osu. */
		padding-left: calc(18px + 0.75rem);
	}
	.btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 4px 10px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-dim);
		font-size: var(--fs-xs);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			opacity var(--t-fast) var(--ease);
	}
	.btn:hover:not(:disabled) {
		background: var(--surface-hover);
		border-color: color-mix(in srgb, var(--accent) 30%, transparent);
		color: var(--text);
	}
	.btn:disabled {
		opacity: 0.55;
		cursor: default;
	}
	/* Hlavní akce: bílá (akcent), žádný gradient ani lesk. */
	.btn.primary {
		border-color: transparent;
		background: var(--accent);
		color: #0e0f12;
		font-weight: 600;
	}
	.btn.primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent) 88%, transparent);
		color: #0e0f12;
	}

	.err {
		margin: 0;
		padding-left: calc(18px + 0.75rem);
		font-size: var(--fs-xs);
		color: var(--danger);
		overflow-wrap: anywhere;
	}
	/* Chybu musí jít označit a zkopírovat. */
	.selectable {
		user-select: text;
		cursor: text;
	}
	/* Nejužší okno: odsazení pod text by tlačítka zbytečně lámalo pod sebe. */
	@media (max-width: 400px) {
		.akce,
		.err {
			padding-left: 0;
		}
	}
</style>
