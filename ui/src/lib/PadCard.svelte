<script lang="ts">
	import Play from 'lucide-svelte/icons/play';
	import RotateCcw from 'lucide-svelte/icons/rotate-ccw';
	import RotateCw from 'lucide-svelte/icons/rotate-cw';
	import Trash2 from 'lucide-svelte/icons/trash-2';
	import { prefersReducedMotion } from 'svelte/motion';
	import { slide } from 'svelte/transition';
	import {
		cilPrirazeni,
		klavesy,
		napovedaKarty,
		odeberOvladac,
		odznaky,
		PRIRAZENI_MS,
		prirazovani,
		pulzPrepinacu,
		rezim,
		vychozi,
		zpet
	} from './klavesy.svelte';
	import { prehraj, trvani, zpomaleni } from './motion';
	import Ovladac from './Ovladac.svelte';
	import {
		instalatorBezi,
		pady,
		prepni,
		sbernicePripravena,
		vyzkousej,
		zapnuto,
		zkusZnovu
	} from './pady.svelte';
	import Prepinac from './Prepinac.svelte';
	import { maVsechnyKlavesy, pocetKlaves, procNeodebrat } from './vstupy';
	import { zive } from './zive.svelte';

	// Karta ovladače jako akordeon: hlavička (pruh a číslo = identita,
	// tečka a přepínač = stav) a u rozbalené karty schéma s klávesami
	// a jeden řádek nápovědy. Kódy chyb a vysvětlivky patří do bubliny
	// a logu, ne do okna. Nic se nepředstírá: „zapnutý" až po potvrzení
	// z ovladače.

	interface Props {
		/** Ovladač 0–3. */
		pad: number;
		rozbalena: boolean;
		onrozbal: () => void;
	}

	let { pad, rozbalena, onrozbal }: Props = $props();

	const p = $derived(pady[pad]!);
	const cislo = $derived(pad + 1);

	/** Přepínač ukazuje přání uživatele, dokud backend neodpoví. */
	const zapnutyPrepinac = $derived(p.prepina ? p.chce : zapnuto(pad));
	// Bez ViGEmBus není co zapnout — mluví pruh nad kartami. Během
	// instalace ovladače taky ne: instalátor by pad odebral.
	const lzePrepnout = $derived(
		sbernicePripravena() && p.state !== 'bus_missing' && p.state !== 'bus_not_running' && !instalatorBezi()
	);
	const pocet = $derived(pocetKlaves(klavesy.vazby, pad));
	const zkratka = $derived(klavesy.zkratka?.nazev ?? 'Zkratka');

	const veta = $derived.by(() => {
		switch (p.state) {
			case 'off':
				// Proč se nezapnul, řekne bublina (celá věta by se utnula).
				return p.detail ? 'Nezapnul se' : 'Vypnutý';
			case 'connecting':
				return 'Zapínám…';
			case 'on':
				// Bez čísla hráče: ViGEmBus ho s víc pady hlásí špatně.
				if (rezim.rezim === 'no_hook') return 'Klávesy nejdou';
				// Přiřazování pozastaví všechny ovladače (jeden režim, OQ 5).
				if (rezim.rezim === 'paused' || rezim.rezim === 'binding') return 'Pozastaveno';
				return 'Zapnutý';
			case 'error':
				return 'Nefunguje';
			default:
				// Chybí / neběží ViGEmBus: vysvětluje pruh nad kartami.
				return 'Vypnutý';
		}
	});

	const bublinaStavu = $derived.by(() => {
		switch (p.state) {
			case 'off':
				// Backend posílá podrobnost jen k odmítnutému zapnutí
				// („počítač se uspává…", „ViGEmBus se právě spustil…").
				return p.detail || 'Virtuální ovladač Xbox 360 — zapne ho přepínač';
			case 'connecting':
				return p.detail || 'Zapínám virtuální ovladač Xbox 360';
			case 'on':
				if (rezim.rezim === 'no_hook')
					return 'Windows nedovolily sledovat klávesnici — klávesy jdou do Windows. Zkus ovladač vypnout a zapnout; podrobnosti jsou v logu.';
				if (rezim.rezim === 'binding') return 'Během přiřazování klávesy hra nic nedostává';
				return rezim.rezim === 'paused'
					? `Klávesy jdou do Windows — ${zkratka} ovladač zase pustí`
					: `Klávesy ovládají virtuální ovladač — ${zkratka} je vrátí Windows`;
			case 'error':
				return p.detail || 'Ovladač nefunguje';
			default:
				return 'ViGEmBus není připravený';
		}
	});
	// Bez kláves to řekne tečka; důvod chyby ale schovat nesmí.
	const bublina = $derived(pocet === 0 ? `Nemá klávesy\n${bublinaStavu}` : bublinaStavu);

	// ── pruh: rozjasní se při stisku kterékoli klávesy ovladače ──
	const BLIK_MS = 120;
	let blik = $state(false);
	let predchozi = 0;
	let casovacBliku: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const drzi = zive.pady[pad]?.drzi ?? 0;
		// Jen nový stisk (nový bit) a jen záblesk (spec 1.2): co je drženo,
		// ukazují čepičky — trvale rozjasněný pruh by při držené páčce nic neříkal.
		if (drzi & ~predchozi) {
			blik = true;
			clearTimeout(casovacBliku);
			casovacBliku = setTimeout(() => (blik = false), BLIK_MS);
		}
		predchozi = drzi;
	});
	$effect(() => () => clearTimeout(casovacBliku));

	// ── nápověda (jediný řádek) ──
	const cil = $derived(cilPrirazeni());
	const cilZde = $derived(cil !== null && cil.pad === pad);
	const napoveda = $derived(napovedaKarty(pad, pocet));

	// „Omezit pohyb": místo ubývajícího proužku text „ještě N s". Časovač
	// běží jen během přiřazování (a jen s touhle volbou).
	// Reaktivně: volbu jde ve Windows přepnout i za běhu okna.
	const omezit = $derived(prefersReducedMotion.current);
	let zbyva = $state(Math.ceil(PRIRAZENI_MS / 1000));
	$effect(() => {
		if (!cilZde || !omezit) return;
		const od = prirazovani.od;
		const tik = () => {
			zbyva = Math.max(0, Math.ceil((PRIRAZENI_MS - (performance.now() - od)) / 1000));
		};
		tik();
		const t = setInterval(tik, 1000);
		return () => clearInterval(t);
	});

	// ── patička: akce na dva kliky ──
	const POTVRZENI_MS = 3000;
	let potvrzuje = $state<'vychozi' | 'odebrat' | null>(null);
	let casovacPotvrzeni: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(casovacPotvrzeni));

	/** Druhý klik do 3 s potvrdí; první jen zčervená ikonu. */
	function dvaKliky(co: 'vychozi' | 'odebrat', akce: () => Promise<unknown>): void {
		clearTimeout(casovacPotvrzeni);
		if (potvrzuje === co) {
			potvrzuje = null;
			void akce();
			return;
		}
		potvrzuje = co;
		casovacPotvrzeni = setTimeout(() => (potvrzuje = null), POTVRZENI_MS);
	}

	// Bublina neaktivního 🗑, nebo '' — odebrat jde jen vypnutý ovladač.
	const neodebrat = $derived(procNeodebrat(p.state, maVsechnyKlavesy(klavesy.vazby, pad)));

	// Klávesy i kartu odebere backend (karta se ukládá, OQ 52).
	async function odebrat(): Promise<void> {
		await odeberOvladac(pad);
	}
</script>

<!-- data-*: stav karty pro styly i pro test okna na skryté ploše
     (data-seq = poslední převzatá změna padu, ne jen odpověď). -->
<section
	class="karta"
	class:rozbalena
	class:stisk={blik}
	data-pad={cislo}
	data-stav={p.state}
	data-rezim={rezim.rezim}
	data-seq={p.seq}
	data-rozbalena={rozbalena}
	aria-label="Ovladač {cislo}"
>
	<span class="pruh" aria-hidden="true"></span>

	<div class="hlava">
		<button
			class="rozbal"
			aria-expanded={rozbalena}
			title={rozbalena ? undefined : 'Ukázat klávesy'}
			onclick={onrozbal}
		>
			<span class="odznak" use:prehraj={{ trida: 'kp-zablesk-odznak', id: odznaky[pad] ?? 0 }}>{cislo}</span>
			<span class="jmeno">Ovladač {cislo}</span>
			<span class="veta" title={bublina}>{veta}</span>
		</button>
		<span class="dot" class:prazdny={pocet === 0} title={bublina}></span>
		<Prepinac
			zapnuto={zapnutyPrepinac}
			zakazano={!lzePrepnout}
			ceka={p.prepina}
			pulz={pulzPrepinacu.id}
			popis="Ovladač {cislo}"
			title={zapnutyPrepinac ? `Vypnout ovladač ${cislo}` : `Zapnout ovladač ${cislo}`}
			onprepni={() => void prepni(pad)}
		/>
	</div>

	{#if p.chybaAkce}
		<p class="err selectable" transition:slide={{ duration: trvani(140), easing: zpomaleni }}>
			{p.chybaAkce}
		</p>
	{/if}

	{#if rozbalena}
		<div class="obsah" transition:slide={{ duration: trvani(160), easing: zpomaleni }}>
			<Ovladac {pad} />

			<div class="pata">
				<p class="napoveda" data-druh={napoveda?.druh} title={napoveda?.titulek} aria-live="polite">
					{#if napoveda}
						<span class="text">
							{napoveda.text}{#if napoveda.druh === 'prirazovani' && omezit}&nbsp;· ještě {zbyva} s{/if}
						</span>
						{#if napoveda.zpet}
							<button class="zpet" onclick={() => void zpet()}>Zpět</button>
						{/if}
					{/if}
				</p>

				<div class="ikony">
					{#if p.state === 'error'}
						<!-- Ovladač po chybě: znovu ověřit sběrnici, nic se
						     nepřipojí (zapne ho až přepínač, princip 11). -->
						<button
							class="ikona"
							title="Zkusit znovu"
							aria-label="Zkusit znovu"
							onclick={() => void zkusZnovu()}
						>
							<RotateCw size={14} strokeWidth={1.9} />
						</button>
					{/if}
					{#if p.state === 'on'}
						<button
							class="ikona"
							title="Vyzkoušet — levá páčka opíše kruh (vidět je třeba v joy.cpl)"
							aria-label="Vyzkoušet"
							aria-disabled={p.testuje}
							onclick={() => {
								if (!p.testuje) void vyzkousej(pad);
							}}
						>
							<Play size={14} strokeWidth={1.9} />
						</button>
					{/if}
					{#if pad === 0}
						<button
							class="ikona"
							class:potvrdit={potvrzuje === 'vychozi'}
							title={potvrzuje === 'vychozi' ? 'Znovu = potvrdit' : 'Výchozí klávesy'}
							aria-label="Výchozí klávesy"
							onclick={() => dvaKliky('vychozi', vychozi)}
						>
							<RotateCcw size={14} strokeWidth={1.9} />
						</button>
					{:else}
						<button
							class="ikona"
							class:potvrdit={potvrzuje === 'odebrat'}
							title={neodebrat || (potvrzuje === 'odebrat' ? 'Znovu = potvrdit' : 'Odebrat ovladač')}
							aria-label="Odebrat ovladač"
							aria-disabled={!!neodebrat}
							onclick={() => {
								if (!neodebrat) dvaKliky('odebrat', odebrat);
							}}
						>
							<Trash2 size={14} strokeWidth={1.9} />
						</button>
					{/if}
				</div>

				{#if cilZde && !omezit}
					<!-- Ubývá 10 s (limit přiřazování v jádře); nové přiřazování
					     proužek spustí znovu, i když už nějaké běží. -->
					{#key prirazovani.id}
						<span
							class="odpocet"
							style:animation-duration="{PRIRAZENI_MS}ms"
							style:animation-delay="-{Math.max(0, Math.round(performance.now() - prirazovani.od))}ms"
							aria-hidden="true"
						></span>
					{/key}
				{/if}
			</div>
		</div>
	{/if}
</section>

<style>
	.karta {
		position: relative;
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		overflow: hidden;
		transition: border-color var(--t-fast) var(--ease);
	}
	/* Barva okraje jen podle významu: porucha červeně, „klávesy nejdou"
	   jantarově. Identitu nese pruh a odznak, ne okraj. */
	.karta[data-stav='error'] {
		border-color: color-mix(in srgb, var(--danger) 32%, var(--border));
	}
	.karta[data-stav='on'][data-rezim='no_hook'] {
		border-color: color-mix(in srgb, var(--warn) 32%, var(--border));
	}

	/* Pruh v barvě ovladače; při stisku jeho klávesy se rozjasní (i u
	   sbalené karty — hráč pozná, kterému ovladači klávesa patří). */
	.pruh {
		position: absolute;
		top: 0;
		bottom: 0;
		left: 0;
		width: 3px;
		background: var(--barva);
		opacity: 0.5;
		transition: opacity 120ms var(--ease);
	}
	.karta.stisk .pruh {
		opacity: 1;
		transition: none;
	}

	.hlava {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.55rem 0.75rem 0.55rem 0.85rem;
	}
	.rozbal {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: center;
		gap: 0.55rem;
		padding: 0;
		border: 0;
		background: none;
		text-align: left;
		cursor: pointer;
	}
	.karta.rozbalena .rozbal {
		cursor: default;
	}
	.odznak {
		flex: none;
		display: grid;
		place-items: center;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--barva);
		color: var(--na-barve);
		font-family: var(--font-mono);
		font-size: 0.7rem;
		font-weight: 500;
		line-height: 1;
	}
	.jmeno {
		flex: none;
		font-size: var(--fs-lg);
		font-weight: 500;
		color: var(--text);
		white-space: nowrap;
	}
	.veta {
		min-width: 0;
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		letter-spacing: 0.03em;
		color: var(--text-faint);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		transition: color var(--t-fast) var(--ease);
	}
	.karta[data-stav='on'] .veta {
		color: var(--text-dim);
	}
	.karta[data-stav='error'] .veta {
		color: var(--danger);
	}
	.karta[data-stav='on'][data-rezim='no_hook'] .veta {
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
	/* Bez kláves: prázdné kolečko (bublina „Nemá klávesy"). */
	.dot.prazdny {
		background: transparent;
		box-shadow: inset 0 0 0 1.5px var(--text-faint);
	}
	.karta[data-stav='connecting'] .dot {
		background: var(--text-dim);
		animation: pulz-tecky 1.6s ease-in-out infinite;
	}
	.karta[data-stav='on'] .dot {
		background: var(--ok);
		box-shadow: var(--glow-ok);
	}
	.karta[data-stav='error'] .dot {
		background: var(--danger);
		box-shadow: var(--glow-danger);
	}
	/* Hook nejde: ovladač existuje, ale klávesy ho neovládají — úkol pro
	   uživatele, jantarová. */
	.karta[data-stav='on'][data-rezim='no_hook'] .dot {
		background: var(--warn);
		box-shadow: var(--glow-warn);
	}
	/* Pozastaveno (zkratkou nebo přiřazováním): ovladač existuje
	   (zelená zůstává pravdivá), ale klávesy teď jdou jinam — tečka
	   zhasne na obrys. */
	.karta[data-stav='on'][data-rezim='paused'] .dot,
	.karta[data-stav='on'][data-rezim='binding'] .dot {
		background: transparent;
		box-shadow: inset 0 0 0 1.5px var(--ok);
	}
	@keyframes pulz-tecky {
		0%,
		100% {
			opacity: 0.35;
		}
		50% {
			opacity: 1;
		}
	}

	.err {
		margin: -0.2rem 0.75rem 0.5rem 0.85rem;
		font-size: var(--fs-xs);
		color: var(--danger);
		overflow-wrap: anywhere;
	}
	/* Chybu musí jít označit a zkopírovat. */
	.selectable {
		user-select: text;
		cursor: text;
	}

	.obsah {
		padding: 0 0.55rem 0.45rem 0.7rem;
	}

	.pata {
		position: relative;
		display: flex;
		align-items: center;
		gap: 0.4rem;
		min-height: 30px;
		padding-top: 2px;
	}
	/* Malé písmo, ne verzálky — nápověda nemá křičet. */
	.napoveda {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: baseline;
		gap: 0.45rem;
		margin: 0;
		padding-left: 0.15rem;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}
	.napoveda .text {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
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
	.napoveda[data-druh='ticha'] {
		color: var(--text-faint);
	}
	.zpet {
		flex: none;
		padding: 0 2px;
		border: 0;
		background: none;
		color: var(--text);
		font-size: var(--fs-xs);
		font-weight: 600;
		text-decoration: underline;
		text-decoration-color: var(--border-strong);
		text-underline-offset: 3px;
		cursor: pointer;
	}
	.zpet:hover {
		text-decoration-color: var(--text-dim);
	}

	.ikony {
		flex: none;
		display: flex;
		gap: 2px;
		margin-left: auto;
	}
	.ikona {
		display: grid;
		place-items: center;
		width: 28px;
		height: 26px;
		padding: 0;
		border: 0;
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-dim);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			opacity var(--t-fast) var(--ease);
	}
	.ikona:hover:not([aria-disabled='true']) {
		background: var(--surface-hover);
		color: var(--text);
	}
	.ikona[aria-disabled='true'] {
		opacity: 0.35;
		cursor: default;
	}
	/* První klik akce na dva kliky: ikona zčervená, druhý potvrdí. */
	.ikona.potvrdit,
	.ikona.potvrdit:hover {
		background: color-mix(in srgb, var(--danger) 16%, transparent);
		color: var(--danger);
	}

	.odpocet {
		position: absolute;
		left: 0.15rem;
		right: 0;
		bottom: 0;
		height: 2px;
		border-radius: 1px;
		background: var(--barva);
		transform-origin: left center;
		animation-name: odpocet;
		animation-timing-function: linear;
		animation-fill-mode: forwards;
	}
	@keyframes odpocet {
		from {
			transform: scaleX(1);
		}
		to {
			transform: scaleX(0);
		}
	}
</style>
