<script lang="ts">
	import Plus from 'lucide-svelte/icons/plus';
	import X from 'lucide-svelte/icons/x';
	import type { Efekt } from './klavesy.svelte';
	import { prehraj } from './motion';
	import type { Klavesa, Vstup } from './smlouva';
	import { bublinaVstupu, jeAlt, type Sviti } from './vstupy';

	// Čepička jednoho vstupu na schématu: krátký název klávesy, která ho
	// ovládá. Stav je vidět z barvy a obrysu (spec 1.3), slova jsou až
	// v bublině. Klik = nahradit klávesu (jako ve hrách), `+` = přidat
	// další, `×` nebo pravý klik = vyprázdnit.

	interface Props {
		vstup: Vstup;
		/** Řádek a sloupec mřížky schématu. */
		r: number;
		s: number;
		klavesy: Klavesa[];
		sviti: Sviti;
		popisek?: string;
		/** Kulatá hlavička páčky (L3/R3). */
		hlavicka?: boolean;
		/** Posun hlavičky v px podle výchylky páčky. */
		posun?: [number, number];
		/** Právě se sem přiřazuje klávesa. */
		cil?: boolean;
		/** Na vstupu jsou všechny klávesy mapování — vyprázdnit nejde (OQ 6). */
		posledni?: boolean;
		efekt?: { druh: Efekt; id: number };
		onprirad: (pridat: boolean) => void;
		onvyprazdni: () => void;
	}

	let {
		vstup,
		r,
		s,
		klavesy,
		sviti,
		popisek,
		hlavicka = false,
		posun,
		cil = false,
		posledni = false,
		efekt,
		onprirad,
		onvyprazdni
	}: Props = $props();

	const TRIDY: Record<Efekt, string> = {
		ulozeno: 'kp-zablesk',
		presunuto: 'kp-zablesk-presun',
		odmitnuto: 'kp-zatreseni'
	};

	const prvni = $derived(klavesy[0]);
	const prazdna = $derived(!prvni);
	// Krátký název má až 5 znaků („Num 8"), u pravého Ctrl 6 („P Ctrl")
	// — při plné velikosti písma by se do čepičky nevešel. Počítá se po
	// znacích (code points), ne po UTF-16 jednotkách.
	const delka = $derived(prvni ? [...prvni.kratky].length : 1);
	const alt = $derived(klavesy.some(jeAlt));
	const bublina = $derived(bublinaVstupu(vstup, klavesy));
	const bublinaKrizku = $derived(posledni ? 'Poslední klávesu nejde odebrat' : 'Vyprázdnit');

	function vyprazdnit(): void {
		if (!prazdna && !posledni) onvyprazdni();
	}

	function pravyKlik(e: MouseEvent): void {
		e.preventDefault();
		vyprazdnit();
	}
</script>

<!-- data-*: stav čepičky pro styly i pro test okna na skryté ploše. -->
<div
	class="cepicka"
	class:hlavicka
	class:ma-popisek={!!popisek}
	class:delsi={delka === 4}
	class:dlouhy={delka >= 5}
	style:grid-row={r}
	style:grid-column={s}
	data-vstup={vstup}
	data-sviti={sviti ?? undefined}
	data-prazdna={prazdna ? '' : undefined}
	data-cil={cil ? '' : undefined}
>
	<button
		class="telo"
		title={bublina}
		aria-label={bublina}
		style:translate={posun ? `${posun[0]}px ${posun[1]}px` : undefined}
		use:prehraj={{ trida: efekt ? TRIDY[efekt.druh] : '', id: efekt?.id ?? 0 }}
		onclick={() => onprirad(false)}
		oncontextmenu={pravyKlik}
	>
		{#if popisek}<span class="popisek">{popisek}</span>{/if}
		<span class="nazev">{prvni ? prvni.kratky : '·'}</span>
		{#if klavesy.length > 1}<span class="vic">+{klavesy.length - 1}</span>{/if}
		{#if alt}<span class="alt"></span>{/if}
	</button>
	{#if !prazdna}
		<!-- Rohy jen pro myš (tabindex −1): z klávesnice by Tab musel
		     projít třikrát víc zastávek. -->
		<button
			class="roh krizek"
			tabindex="-1"
			title={bublinaKrizku}
			aria-label={bublinaKrizku}
			aria-disabled={posledni}
			onclick={vyprazdnit}
		>
			<X size={9} strokeWidth={2.6} />
		</button>
		<button
			class="roh plus"
			tabindex="-1"
			title="Přidat další klávesu"
			aria-label="Přidat další klávesu"
			onclick={() => onprirad(true)}
		>
			<Plus size={9} strokeWidth={2.6} />
		</button>
	{/if}
</div>

<style>
	.cepicka {
		position: relative;
		width: var(--cep);
		height: var(--cep);
	}

	.telo {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		padding: 0;
		border: 1px solid color-mix(in srgb, var(--barva) 30%, transparent);
		border-radius: var(--radius);
		/* Tmavý povrch: čepička se odliší od karty, i když nesvítí. */
		background: rgba(8, 9, 12, 0.5);
		color: var(--text);
		/* Symboly (␣ ↵ ⌫ ⇧) Space Grotesk nemá — vykreslí je systémové
		   písmo z Windows, žádný soubor navíc (CSP). */
		font-family: 'Space Grotesk', 'Segoe UI Symbol', 'Segoe UI', sans-serif;
		font-size: calc(var(--cep) * 0.34);
		font-weight: 500;
		line-height: 1;
		cursor: pointer;
		overflow: hidden;
		transition:
			background-color var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease),
			translate 70ms var(--ease);
	}
	.hlavicka .telo {
		border-radius: 50%;
	}
	.telo:hover {
		background: color-mix(in srgb, var(--barva) 10%, rgba(8, 9, 12, 0.5));
		border-color: color-mix(in srgb, var(--barva) 55%, transparent);
	}

	.nazev {
		max-width: 100%;
		padding: 0 2px;
		white-space: nowrap;
		overflow: hidden;
	}
	.ma-popisek .nazev {
		margin-top: calc(var(--cep) * 0.2);
	}
	.delsi .nazev {
		font-size: calc(var(--cep) * 0.29);
	}
	.dlouhy .nazev {
		font-size: calc(var(--cep) * 0.24);
		letter-spacing: -0.02em;
	}
	/* Popisek tlačítka (A, LB, ◀) v rohu — malý a tichý, hlavní je klávesa. */
	.popisek {
		position: absolute;
		top: 2px;
		left: 3px;
		font-family: var(--font-mono);
		font-size: calc(var(--cep) * 0.23);
		font-weight: 400;
		color: var(--text-faint);
	}
	.vic {
		position: absolute;
		top: 2px;
		right: 3px;
		font-family: var(--font-mono);
		font-size: calc(var(--cep) * 0.23);
		font-weight: 400;
		color: var(--text-dim);
	}
	/* Alt na vstupu: jen upozornění (Alt+Tab při hraní nepůjde). */
	.alt {
		position: absolute;
		bottom: 3px;
		left: 3px;
		width: 4px;
		height: 4px;
		border-radius: 50%;
		background: var(--warn);
	}

	.cepicka[data-prazdna] .telo {
		border-style: dashed;
		border-color: var(--border-strong);
		background: transparent;
		color: var(--text-faint);
	}

	/* Klávesa je dole, ale hra vstup nedostává (vypnutý ovladač, pauza,
	   poražený směr A+D): obrys a slabá výplň — „náhled". */
	.cepicka[data-sviti='nahled'] .telo {
		border-color: var(--barva);
		box-shadow: inset 0 0 0 0.5px var(--barva);
		background: color-mix(in srgb, var(--barva) 15%, transparent);
	}
	/* Hra vstup dostává: plná barva ovladače. Bez glow — ten patří jen
	   stavovým barvám. */
	.cepicka[data-sviti='hra'] .telo {
		border-color: var(--barva);
		background: var(--barva);
		color: var(--na-barve);
	}
	.cepicka[data-sviti='hra'] .popisek,
	.cepicka[data-sviti='hra'] .vic {
		color: color-mix(in srgb, var(--na-barve) 70%, transparent);
	}

	/* Přiřazuje se sem: pulz v barvě ovladače. S „Omezit pohyb" pulz
	   zmizí a zůstane pevná výplň a plný obrys. */
	.cepicka[data-cil] .telo {
		border-style: solid;
		border-color: var(--barva);
		background: color-mix(in srgb, var(--barva) 24%, transparent);
		color: var(--text);
		animation: kp-pulz 1.1s ease-in-out infinite;
	}
	/* Jednorázové animace z app.css (třídy přidává `prehraj`) by pulz
	   přebil svou vyšší specificitou — odmítnutá klávesa se ale hlásí
	   právě na pulzující čepičce. Zatřesení jede vedle pulzu, záblesk
	   pulz na 160 ms vystřídá. */
	.cepicka[data-cil] .telo:global(.kp-zatreseni) {
		animation:
			kp-zatreseni 160ms linear,
			kp-pulz 1.1s ease-in-out infinite;
	}
	.cepicka[data-cil] .telo:global(.kp-zablesk) {
		animation: kp-zablesk 160ms ease-out;
	}

	.roh {
		position: absolute;
		z-index: 2;
		display: grid;
		place-items: center;
		width: 14px;
		height: 14px;
		padding: 0;
		border: 1px solid var(--border-strong);
		border-radius: 50%;
		background: var(--popover);
		color: var(--text-dim);
		cursor: pointer;
		opacity: 0;
		pointer-events: none;
		transition:
			opacity var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease);
	}
	.cepicka:hover .roh {
		opacity: 1;
		pointer-events: auto;
	}
	.roh:hover {
		color: var(--text);
	}
	.krizek {
		top: -5px;
		right: -5px;
	}
	.krizek:hover {
		color: var(--danger);
	}
	.plus {
		right: -5px;
		bottom: -5px;
	}
	.cepicka:hover .roh[aria-disabled='true'] {
		opacity: 0.35;
		cursor: default;
	}
	.roh[aria-disabled='true']:hover {
		color: var(--text-dim);
	}
</style>
