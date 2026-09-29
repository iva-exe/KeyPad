<script lang="ts">
	import Check from 'lucide-svelte/icons/check';
	import Copy from 'lucide-svelte/icons/copy';
	import Download from 'lucide-svelte/icons/download';
	import Gamepad2 from 'lucide-svelte/icons/gamepad-2';
	import RotateCw from 'lucide-svelte/icons/rotate-cw';
	import { slide } from 'svelte/transition';
	import { trvani, zpomaleni } from './motion';
	import { nainstalujVigem, odkazV, pad, vyzkousejPacku, zkusZnovu } from './pad.svelte';

	// Karta virtuálního gamepadu: jedna věta o stavu, číslo hráče
	// a nejvýš dvě tlačítka — jen ta, která v daném stavu něco udělají.
	// Nic se nepředstírá: „připojený" až po potvrzení z ovladače.

	const veta = $derived(
		{
			connecting: 'Připojuji virtuální ovladač Xbox 360…',
			connected: 'Virtuální ovladač Xbox 360 je připojený.',
			bus_missing: 'Chybí ovladač ViGEmBus — bez něj virtuální gamepad nevznikne.',
			// Co přesně s ním je (vypnutý, čeká na restart…), říká rada pod tím.
			bus_not_running: 'Ovladač ViGEmBus neběží — bez něj virtuální gamepad nevznikne.',
			error: 'Virtuální ovladač nefunguje.',
			suspended: 'Virtuální ovladač je odpojený kvůli spánku počítače.'
		}[pad.state]
	);

	// Podrobnost: u chyby a připojování důvod od backendu, u neběžícího
	// ViGEmBus rada od backendu (tatáž jako v instalátoru), u chybějícího
	// vysvětlení PŘEDEM, co tlačítko udělá (princip 8).
	const podrobnost = $derived.by(() => {
		if (pad.state === 'bus_missing') {
			return pad.instalatorBezi
				? 'Instalátor běží — dokonči instalaci v jeho okně. KeyPad pak ovladač sám ověří a připojí.'
				: 'Instalátor KeyPadu stáhne oficiální ViGEmBus (Nefarius Software Solutions), ověří, že je to přesně ten správný soubor, a spustí ho. Windows se zeptají na oprávnění správce — KeyPad sám práva správce nemá.';
		}
		if (pad.state === 'suspended') {
			// Oznámení o probuzení Windows někdy nepošlou (Modern Standby).
			return 'Po probuzení se připojí sám. Když se to nestane, klikni na Připojit znovu.';
		}
		if (pad.state === 'connected') return '';
		return pad.detail;
	});

	// Adresu ruční instalace pošle backend v textu (chyba tlačítka, nebo
	// rada u neběžícího ViGEmBus) — okno si ji jen najde.
	const odkaz = $derived(odkazV(pad.chybaAkce) ?? odkazV(podrobnost));
	const odkazVChybe = $derived(odkazV(pad.chybaAkce) !== null);

	let zkopirovano = $state(false);
	let casovac: ReturnType<typeof setTimeout> | undefined;

	async function kopirujOdkaz() {
		if (!odkaz) return;
		try {
			await navigator.clipboard.writeText(odkaz);
			zkopirovano = true;
			clearTimeout(casovac);
			casovac = setTimeout(() => (zkopirovano = false), 1600);
		} catch {
			// Schránka nejde (oprávnění WebView2) — odkaz jde označit ručně.
		}
	}
</script>

<!-- data-seq: pořadí poslední převzaté změny — podle něj jde zvenku
     (test přes CDP) poznat, že dorazila událost, ne jen odpověď. -->
<section class="card" data-stav={pad.state} data-seq={pad.seq} aria-live="polite">
	<header class="head">
		<Gamepad2 size={16} strokeWidth={1.75} />
		<span class="label-tech">virtuální gamepad</span>
		{#if pad.state === 'connected' && pad.player}
			<span class="hrac" title="Číslo hráče, které Windows ovladači přidělily (slot XInput)">
				hráč <b class="value-mono">{pad.player}</b>
			</span>
		{/if}
	</header>

	<p class="veta">{veta}</p>
	{#if podrobnost}
		<!-- Důvod a rada od backendu jdou označit a zkopírovat (hledat,
		     poslat dál); vlastní vysvětlivky okna ne. -->
		<p
			class="detail"
			class:selectable={pad.state === 'error' || pad.state === 'bus_not_running'}
			transition:slide={{ duration: trvani(140), easing: zpomaleni }}
		>
			{podrobnost}
		</p>
		{#if odkaz && !odkazVChybe}
			{@render kopirovat()}
		{/if}
	{/if}

	{#if pad.state === 'connected'}
		<div class="akce">
			<button class="btn" disabled={pad.testuje} onclick={() => void vyzkousejPacku()}>
				{pad.testuje ? 'páčka opisuje kruh…' : 'Vyzkoušet páčku'}
			</button>
			<span class="hint">Levá páčka opíše kruh — vidět je třeba v joy.cpl.</span>
		</div>
	{:else if pad.state === 'bus_missing'}
		<div class="akce">
			<button
				class="btn primary"
				disabled={pad.spoustiInstalator || pad.instalatorBezi}
				onclick={() => void nainstalujVigem()}
			>
				<Download size={15} strokeWidth={1.9} />
				{pad.spoustiInstalator ? 'spouštím…' : pad.instalatorBezi ? 'instalátor běží…' : 'Nainstalovat ViGEmBus'}
			</button>
			<button class="btn" onclick={() => void zkusZnovu()}>
				<RotateCw size={14} strokeWidth={1.9} />
				Zkusit znovu
			</button>
		</div>
	{:else if pad.state === 'error' || pad.state === 'bus_not_running'}
		<!-- Neběžící ViGEmBus: instalace by nepomohla (ovladač v systému
		     je), jen „Zkusit znovu" po zapnutí / restartu. -->
		<div class="akce">
			<button class="btn" onclick={() => void zkusZnovu()}>
				<RotateCw size={14} strokeWidth={1.9} />
				Zkusit znovu
			</button>
		</div>
	{:else if pad.state === 'suspended'}
		<!-- Kdyby oznámení o probuzení nepřišlo, pad by jinak zůstal
		     odpojený navždy; backend tohle kliknutí bere jako probuzení. -->
		<div class="akce">
			<button class="btn" onclick={() => void zkusZnovu()}>
				<RotateCw size={14} strokeWidth={1.9} />
				Připojit znovu
			</button>
		</div>
	{/if}

	{#if pad.chybaAkce}
		<div class="err" transition:slide={{ duration: trvani(140), easing: zpomaleni }}>
			<p class="selectable">{pad.chybaAkce}</p>
			{#if odkazVChybe}
				{@render kopirovat()}
			{/if}
		</div>
	{/if}
</section>

{#snippet kopirovat()}
	<button class="kopie" onclick={() => void kopirujOdkaz()}>
		{#if zkopirovano}
			<Check size={13} strokeWidth={2} /> zkopírováno
		{:else}
			<Copy size={13} strokeWidth={1.9} /> kopírovat odkaz
		{/if}
	</button>
{/snippet}

<style>
	.card {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		padding: 0.95rem 1rem 1rem;
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		transition: border-color var(--t-fast) var(--ease);
	}
	/* Barva jen podle významu: zelený nádech teprve u skutečně
	   připojeného ovladače, červený u poruchy. */
	.card[data-stav='connected'] {
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

	.head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--text-dim);
	}
	.hrac {
		margin-left: auto;
		padding: 1px 8px;
		border: 1px solid color-mix(in srgb, var(--ok) 35%, transparent);
		border-radius: 999px;
		color: var(--text-dim);
		font-size: var(--fs-xs);
	}
	.hrac b {
		color: var(--ok);
		font-weight: 500;
	}

	.veta {
		margin: 0;
		font-size: var(--fs-xl);
		color: var(--text);
		text-wrap: pretty;
	}
	.detail {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
		text-wrap: pretty;
	}

	.akce {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.75rem;
		margin-top: 0.25rem;
	}
	.hint {
		flex: 1 1 12rem;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.btn {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 6px 13px;
		border: 1px solid var(--border-strong);
		border-radius: var(--radius-sm);
		background: var(--surface);
		color: var(--text);
		font-size: var(--fs-sm);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			border-color var(--t-fast) var(--ease),
			opacity var(--t-fast) var(--ease);
	}
	.btn:hover:not(:disabled) {
		background: var(--surface-hover);
		border-color: color-mix(in srgb, var(--accent) 30%, transparent);
	}
	.btn:disabled {
		opacity: 0.6;
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
	}

	.err {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.35rem;
	}
	.err p {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--danger);
		overflow-wrap: anywhere;
	}
	/* Chybu i s odkazem musí jít označit a zkopírovat. */
	.selectable {
		user-select: text;
		cursor: text;
	}
	.kopie {
		/* I pod podrobností přímo v kartě (sloupec) jen na šířku obsahu. */
		align-self: flex-start;
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 2px 8px;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		background: none;
		color: var(--text-dim);
		font-family: var(--font-mono);
		font-size: var(--fs-2xs);
		cursor: pointer;
		transition:
			background var(--t-fast) var(--ease),
			color var(--t-fast) var(--ease);
	}
	.kopie:hover {
		background: var(--surface-hover);
		color: var(--text);
	}
</style>
