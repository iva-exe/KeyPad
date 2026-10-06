<script lang="ts">
	import ExternalLink from 'lucide-svelte/icons/external-link';
	import Play from 'lucide-svelte/icons/play';
	import ScrollText from 'lucide-svelte/icons/scroll-text';
	import { onMount } from 'svelte';
	import { fly } from 'svelte/transition';
	import { aplikace } from './aplikace.svelte';
	import {
		efektZkratky,
		klavesy,
		napovedaZkratky,
		nastav,
		nastaveni,
		priradZkratku,
		prirazuje,
		prirazujeZkratku,
		ukazkaZvuku
	} from './klavesy.svelte';
	import { prehraj, trvani, zpomaleni } from './motion';
	import { otevri } from './pady.svelte';
	import Prepinac from './Prepinac.svelte';
	import { textChyby, zavolej } from './tauri';
	import { varovaniZkratky } from './vstupy';

	// Nastavení a o aplikaci (Fáze 7, Z6): nahoře volby — zvuk (tatáž
	// hodnota jako „✓ Zvuk" v nabídce ikony), jedna klávesa pro víc vstupů
	// a zkratka pozastavení —, pod nimi verze, credit ViGEmBus (bez něj by
	// KeyPad nebyl) a log. „Vždy navrchu" tu není schválně: okno přes hru
	// by zakrylo hru i stream (OQ 68). Výchozí klávesy jsou ↺ na kartě.

	interface Props {
		/** Zavřít (Esc, klik mimo). */
		onzavri: () => void;
		/** Tlačítko, které panel otevřelo — klik na něj panel přepíná sám. */
		kotva: HTMLElement | undefined;
	}

	let { onzavri, kotva }: Props = $props();

	let panel: HTMLElement | undefined = $state();
	let chyba = $state('');
	/** Proč ▷ nehraje (simulace, vypnutý zvuk) — do bubliny. */
	let ukazka = $state('');

	onMount(() => {
		// Fokus do panelu: Esc pak funguje hned a čtečka ho přečte.
		panel?.focus();
	});

	function mimo(e: PointerEvent) {
		const cil = e.target as Node | null;
		if (!cil || panel?.contains(cil) || kotva?.contains(cil)) return;
		onzavri();
	}

	function klavesa(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		// Během přiřazování (i zkratky odsud) Esc ruší jen přiřazování —
		// to dělá App.svelte; panel zůstane otevřený.
		if (prirazuje()) return;
		e.preventDefault();
		onzavri();
		kotva?.focus();
	}

	async function odkaz() {
		chyba = await otevri('vigembus');
	}

	async function log() {
		try {
			await zavolej('open_log_dir');
			chyba = '';
		} catch (e) {
			chyba = textChyby(e);
		}
	}

	async function prepniZvuk() {
		chyba = await nastav('zvuk', !nastaveni.zvuk);
		ukazka = '';
	}

	async function prepniSdileni() {
		chyba = await nastav('sdilene_klavesy', !nastaveni.sdilene_klavesy);
	}

	async function prehrajUkazku() {
		ukazka = await ukazkaZvuku();
	}

	const zkratka = $derived(klavesy.zkratka);
	const varovani = $derived(varovaniZkratky(zkratka, klavesy.zkratkaMimo));
	const prirazujeSem = $derived(prirazujeZkratku());
	const napoveda = $derived(napovedaZkratky());
</script>

<svelte:window onpointerdown={mimo} onkeydown={klavesa} />

<div
	class="about"
	role="dialog"
	aria-label="Nastavení a o aplikaci"
	tabindex="-1"
	bind:this={panel}
	transition:fly={{ y: -6, duration: trvani(140), easing: zpomaleni }}
>
	<div class="volby">
		<div class="volba" data-volba="zvuk">
			<span class="nazev" title="Tón při pozastavení a pokračování (zkratkou, nabídkou ikony, pojistkou)">Zvuk</span>
			<button
				class="ukazka"
				title={ukazka || 'Ukázka: pozastavení a pokračování'}
				aria-label="Ukázka zvuku"
				data-ukazka={ukazka || undefined}
				onclick={() => void prehrajUkazku()}
			>
				<Play size={12} strokeWidth={1.9} />
			</button>
			<Prepinac volba zapnuto={nastaveni.zvuk} popis="Zvuk" onprepni={() => void prepniZvuk()} />
		</div>

		<div class="volba" data-volba="sdilene_klavesy">
			<span class="nazev" title="Klávesa přiřazená podruhé zůstane i u původního vstupu (oranžově)">
				Jedna klávesa pro víc vstupů
			</span>
			<Prepinac
				volba
				zapnuto={nastaveni.sdilene_klavesy}
				popis="Jedna klávesa pro víc vstupů"
				title="Klávesa přiřazená podruhé zůstane i u původního vstupu (oranžově)"
				onprepni={() => void prepniSdileni()}
			/>
		</div>

		<div class="volba" data-volba="zkratka">
			<span class="nazev" title="Klávesa, která pozastaví a pustí hru — jen F1–F24 (kromě F4, ať funguje Alt+F4), Scroll Lock nebo Pause">Pauza</span>
			<!-- data-zkratka: klik sem není „klik jinam" (App.svelte). -->
			<button
				class="cepicka"
				data-zkratka
				data-cil={prirazujeSem ? '' : undefined}
				title={varovani || 'Klikni a stiskni F1–F24 (ne F4), Scroll Lock nebo Pause'}
				aria-label={`Zkratka pozastavení: ${zkratka?.nazev ?? '—'}${varovani ? ` — ${varovani}` : ''}`}
				use:prehraj={{
					trida: efektZkratky.druh === 'odmitnuto' ? 'kp-zatreseni' : 'kp-zablesk',
					id: efektZkratky.id
				}}
				onclick={() => void priradZkratku()}
			>
				<span class="klavesa">{zkratka?.nazev ?? '—'}</span>
				{#if varovani}<span class="tecka" data-varovani></span>{/if}
			</button>
		</div>
		{#if napoveda}
			<p class="napoveda" data-druh={napoveda.druh} aria-live="polite">{napoveda.text}</p>
		{/if}
	</div>

	<div class="radek">
		<span class="label-tech">keypad</span>
		<span class="value-mono ver" title={aplikace.verze}>{aplikace.verze}</span>
	</div>

	<div class="radek credit">
		<span class="label-tech">ovladač</span>
		<!-- Text i adresa z backendu (updater::vigembus); klik otevírá
		     backend podle jména, adresu z okna nebere. -->
		<button
			class="odkaz"
			title={aplikace.vigembusAdresa.replace(/^https:\/\//, '') || undefined}
			onclick={() => void odkaz()}
		>
			<span>{aplikace.vigembusCredit || 'ViGEmBus'}</span>
			<ExternalLink size={12} strokeWidth={1.9} />
		</button>
		{#if aplikace.vigembusLicence}
			<span class="lic value-mono">{aplikace.vigembusLicence}</span>
		{/if}
	</div>

	<div class="pata">
		{#if chyba}
			<span class="err" title={chyba}>{chyba}</span>
		{/if}
		<button
			class="log"
			title={aplikace.cestaLogu ? `Otevřít složku s logem — ${aplikace.cestaLogu}` : 'Otevřít složku s logem'}
			onclick={() => void log()}
		>
			<ScrollText size={13} strokeWidth={1.75} />
			<span>log</span>
		</button>
	</div>
</div>

<style>
	.about {
		position: fixed;
		top: 42px;
		right: 12px;
		z-index: 10;
		width: min(300px, calc(100vw - 24px));
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		padding: 0.8rem 0.9rem 0.6rem;
		/* Neprůhledné: pod panelem je karta a text by prosvítal. */
		background: var(--popover);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-lg);
		box-shadow: 0 10px 28px rgba(0, 0, 0, 0.45);
		outline: none;
	}

	/* ── volby ── */
	.volby {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding-bottom: 0.7rem;
		border-bottom: 1px dotted var(--border-strong);
	}
	.volba {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-height: 26px;
	}
	.volba .nazev {
		flex: 1;
		min-width: 0;
		font-size: var(--fs-sm);
		color: var(--text);
	}
	.ukazka {
		display: grid;
		place-items: center;
		width: 26px;
		height: 24px;
		padding: 0;
		border: 0;
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-dim);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease);
	}
	.ukazka:hover {
		background: var(--surface-hover);
		color: var(--text);
	}
	/* Čepička se zkratkou — jako čepička vstupu, jen širší (celý název). */
	.cepicka {
		position: relative;
		flex: none;
		max-width: 9rem;
		min-width: 3.2rem;
		height: 26px;
		padding: 0 10px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius);
		background: rgba(8, 9, 12, 0.5);
		color: var(--text);
		font-size: var(--fs-xs);
		font-weight: 500;
		cursor: pointer;
		transition:
			background-color var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease);
	}
	.cepicka:hover {
		border-color: color-mix(in srgb, var(--accent) 45%, transparent);
	}
	.cepicka .klavesa {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* Přiřazuje se zkratka: pulz jako u čepiček vstupů (bíle — zkratka
	   nepatří žádnému ovladači). */
	.cepicka[data-cil] {
		--barva: var(--accent);
		border-color: var(--accent);
		animation: kp-pulz 1.1s ease-in-out infinite;
	}
	.cepicka[data-cil]:global(.kp-zatreseni) {
		animation:
			kp-zatreseni 160ms linear,
			kp-pulz 1.1s ease-in-out infinite;
	}
	.cepicka:global(.kp-zablesk) {
		--barva: var(--accent);
	}
	/* F12 nebo zkratka mimo seznam (ručně v config.json): varování, ne
	   chyba — jantarová tečka jako u Altu na čepičce vstupu. */
	.tecka {
		position: absolute;
		bottom: 3px;
		left: 3px;
		width: 4px;
		height: 4px;
		border-radius: 50%;
		background: var(--warn);
	}
	.napoveda {
		margin: -0.15rem 0 0;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}
	.napoveda[data-druh='prirazovani'] {
		color: var(--text);
	}
	.napoveda[data-druh='odmitnuto'] {
		color: var(--warn);
	}
	.napoveda[data-druh='chyba'] {
		color: var(--danger);
	}

	.radek {
		display: grid;
		grid-template-columns: 4.6rem 1fr;
		align-items: baseline;
		gap: 0.15rem 0.6rem;
	}
	.ver {
		font-size: var(--fs-2xs);
		color: var(--text-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.credit .lic {
		grid-column: 2;
		font-size: var(--fs-3xs);
		color: var(--text-faint);
	}
	/* Inline: dlouhé jméno se zalomí a ikona jde za posledním slovem,
	   ne do rohu. */
	.odkaz {
		display: inline;
		justify-self: start;
		padding: 0;
		border: 0;
		background: none;
		color: var(--text);
		font-size: var(--fs-sm);
		text-align: left;
		cursor: pointer;
		transition: color var(--t-fast) var(--ease);
	}
	.odkaz span {
		text-decoration: underline;
		text-decoration-color: var(--border-strong);
		text-underline-offset: 3px;
		transition: text-decoration-color var(--t-fast) var(--ease);
	}
	.odkaz :global(svg) {
		margin-left: 0.3rem;
		vertical-align: -1px;
		color: var(--text-faint);
	}
	.odkaz:hover span {
		text-decoration-color: var(--text-dim);
	}

	.pata {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding-top: 0.5rem;
		border-top: 1px dotted var(--border-strong);
	}
	.err {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--fs-xs);
		color: var(--danger);
	}
	.log {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 3px 8px;
		border: 1px solid transparent;
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-faint);
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease);
	}
	.log:hover {
		background: var(--surface-hover);
		border-color: var(--border);
		color: var(--text);
	}
</style>
